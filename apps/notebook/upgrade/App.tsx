import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { relaunch } from "@tauri-apps/plugin-process";
import { check } from "@tauri-apps/plugin-updater";
import { AlertTriangle, Check, Circle, Loader2, Notebook } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import type { NotebookStatus, UpgradeStep } from "./types";

type Phase = "review" | "progress";

interface StepInfo {
  id: string;
  label: string;
  status: "pending" | "in_progress" | "completed" | "failed";
  error?: string;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function NotebookRow({
  notebook,
  onAbort,
  aborting,
}: {
  notebook: NotebookStatus;
  onAbort: () => void;
  aborting: boolean;
}) {
  const isBusy = notebook.kernel_status === "busy";
  const hasKernel = notebook.kernel_status !== null;

  return (
    <div className="flex items-center justify-between py-2 px-3 rounded-md bg-muted/50">
      <div className="flex items-center gap-3">
        <Notebook className="h-4 w-4 text-muted-foreground" />
        <div className="flex flex-col">
          <span className="text-sm font-medium">{notebook.display_name}</span>
          {notebook.is_dirty && (
            <span className="text-xs text-muted-foreground">Unsaved changes</span>
          )}
        </div>
      </div>
      <div className="flex items-center gap-3">
        {hasKernel && (
          <div className="flex items-center gap-1.5">
            {isBusy ? (
              <Circle className="h-2.5 w-2.5 fill-amber-500 text-amber-500" />
            ) : (
              <Circle className="h-2.5 w-2.5 fill-green-500 text-green-500" />
            )}
            <span className="text-xs text-muted-foreground capitalize">
              {notebook.kernel_status?.replace("_", " ")}
            </span>
          </div>
        )}
        {isBusy && (
          <Button
            variant="outline"
            size="sm"
            onClick={onAbort}
            disabled={aborting}
            className="h-7 text-xs"
          >
            {aborting ? <Loader2 className="h-3 w-3 animate-spin" /> : "Stop"}
          </Button>
        )}
      </div>
    </div>
  );
}

function StepRow({ step }: { step: StepInfo }) {
  return (
    <div className="flex items-center gap-3 py-1.5">
      {step.status === "completed" && <Check className="h-4 w-4 text-green-600" />}
      {step.status === "in_progress" && (
        <Loader2 className="h-4 w-4 animate-spin text-foreground" />
      )}
      {step.status === "pending" && <Circle className="h-4 w-4 text-muted-foreground/40" />}
      {step.status === "failed" && <AlertTriangle className="h-4 w-4 text-red-500" />}
      <span
        className={cn(
          "text-sm",
          step.status === "completed" && "text-muted-foreground",
          step.status === "in_progress" && "text-foreground font-medium",
          step.status === "pending" && "text-muted-foreground",
          step.status === "failed" && "text-red-500",
        )}
      >
        {step.label}
      </span>
    </div>
  );
}

export default function App() {
  const [phase, setPhase] = useState<Phase>("review");
  const [notebooks, setNotebooks] = useState<NotebookStatus[]>([]);
  const [abortingKernels, setAbortingKernels] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [downloadProgress, setDownloadProgress] = useState<{
    total: number | null;
    downloaded: number;
  } | null>(null);
  const [steps, setSteps] = useState<StepInfo[]>([
    { id: "downloading", label: "Downloading update", status: "pending" },
    { id: "saving", label: "Saving notebooks", status: "pending" },
    { id: "stopping", label: "Stopping runtimes", status: "pending" },
    { id: "closing", label: "Closing windows", status: "pending" },
    { id: "upgrading", label: "Upgrading daemon", status: "pending" },
    { id: "ready", label: "Ready to restart", status: "pending" },
  ]);

  // Fetch notebook status on mount
  useEffect(() => {
    invoke<NotebookStatus[]>("get_upgrade_notebook_status")
      .then(setNotebooks)
      .catch((e) => setError(String(e)));
  }, []);

  // Listen for progress events
  useEffect(() => {
    const unlistenProgress = listen<UpgradeStep>("upgrade:progress", (event) => {
      const payload = event.payload;

      setSteps((prev) => {
        const newSteps = [...prev];

        // Mark all steps as completed up to the current one
        const stepMap: Record<string, number> = {
          saving_notebooks: 1,
          stopping_runtimes: 2,
          closing_windows: 3,
          upgrading_daemon: 4,
          ready: 5,
        };

        if (payload.step === "failed") {
          // Mark current in-progress step as failed
          const failedStep = newSteps.find((s) => s.status === "in_progress");
          if (failedStep) {
            failedStep.status = "failed";
            failedStep.error = payload.error;
          }
          setError(payload.error);
          return newSteps;
        }

        const currentIndex = stepMap[payload.step];
        if (currentIndex !== undefined) {
          for (let i = 0; i < newSteps.length; i++) {
            if (i < currentIndex) {
              newSteps[i].status = "completed";
            } else if (i === currentIndex) {
              newSteps[i].status = payload.step === "ready" ? "completed" : "in_progress";
            } else {
              newSteps[i].status = "pending";
            }
          }
        }

        return newSteps;
      });
    });

    return () => {
      unlistenProgress.then((unlisten) => unlisten()).catch(() => {});
    };
  }, []);

  const hasBusyKernels = notebooks.some((nb) => nb.kernel_status === "busy");
  const isReady = steps.every((s) => s.status === "completed");
  const hasFailed = steps.some((s) => s.status === "failed");

  const handleAbort = useCallback(async (windowLabel: string) => {
    setAbortingKernels((prev) => new Set(prev).add(windowLabel));
    try {
      await invoke("abort_kernel_for_upgrade", { windowLabel });
      // Refresh notebook status
      const updated = await invoke<NotebookStatus[]>("get_upgrade_notebook_status");
      setNotebooks(updated);
    } catch (e) {
      setError(String(e));
    } finally {
      setAbortingKernels((prev) => {
        const next = new Set(prev);
        next.delete(windowLabel);
        return next;
      });
    }
  }, []);

  const handleContinue = useCallback(async () => {
    setPhase("progress");
    setError(null);

    // Step 0: Download and install the update
    setSteps((prev) => prev.map((s, i) => (i === 0 ? { ...s, status: "in_progress" } : s)));

    try {
      const update = await check();
      if (!update) {
        throw new Error(
          "The update is no longer available. Please close this window and check for updates again.",
        );
      }

      let downloaded = 0;
      await update.downloadAndInstall((event) => {
        switch (event.event) {
          case "Started":
            setDownloadProgress({
              total: event.data.contentLength ?? null,
              downloaded: 0,
            });
            break;
          case "Progress":
            downloaded += event.data.chunkLength;
            setDownloadProgress((prev) => ({
              total: prev?.total ?? null,
              downloaded,
            }));
            break;
          case "Finished":
            break;
        }
      });

      // Mark download as completed
      setSteps((prev) => prev.map((s, i) => (i === 0 ? { ...s, status: "completed" } : s)));
      setDownloadProgress(null);
    } catch (e) {
      setSteps((prev) =>
        prev.map((s) =>
          s.status === "in_progress" ? { ...s, status: "failed", error: String(e) } : s,
        ),
      );
      setError(String(e));
      return;
    }

    // Steps 1-5: Save, stop, close, upgrade daemon, ready
    try {
      await invoke("run_upgrade");
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const handleRestart = useCallback(async () => {
    await relaunch();
  }, []);

  return (
    <div className="flex min-h-screen flex-col items-center justify-center bg-background p-8">
      <div className="w-full max-w-md space-y-6">
        {phase === "review" && (
          <>
            <div className="text-center space-y-2">
              <h1 className="text-2xl font-semibold tracking-tight">Update Ready</h1>
              <p className="text-sm text-muted-foreground">Review your notebooks before updating</p>
            </div>

            {notebooks.length > 0 ? (
              <div className="space-y-2">
                <p className="text-xs text-muted-foreground uppercase tracking-wide">
                  Open Notebooks
                </p>
                <div className="space-y-1.5 max-h-64 overflow-y-auto">
                  {notebooks.map((nb) => (
                    <NotebookRow
                      key={nb.window_label}
                      notebook={nb}
                      onAbort={() => handleAbort(nb.window_label)}
                      aborting={abortingKernels.has(nb.window_label)}
                    />
                  ))}
                </div>
              </div>
            ) : (
              <div className="text-center py-4 text-sm text-muted-foreground">
                No notebooks open
              </div>
            )}

            {hasBusyKernels && (
              <div className="flex items-start gap-2 p-3 rounded-md bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800">
                <AlertTriangle className="h-4 w-4 text-amber-600 dark:text-amber-400 shrink-0 mt-0.5" />
                <p className="text-sm text-amber-800 dark:text-amber-200">
                  Some notebooks have running code. Stop or wait before continuing.
                </p>
              </div>
            )}

            <div className="text-center text-xs text-muted-foreground">
              Runtimes will restart after update
            </div>

            <Button onClick={handleContinue} disabled={hasBusyKernels} className="w-full" size="lg">
              {hasBusyKernels ? "Stop busy runtimes first" : "Continue Update"}
            </Button>
          </>
        )}

        {phase === "progress" && (
          <>
            <div className="text-center space-y-2">
              <h1 className="text-2xl font-semibold tracking-tight">
                {isReady ? "Update Complete" : hasFailed ? "Update Failed" : "Updating..."}
              </h1>
              <p className="text-sm text-muted-foreground">
                {isReady
                  ? "Ready to restart with the new version"
                  : hasFailed
                    ? "Something went wrong"
                    : "Please wait while we prepare the update"}
              </p>
            </div>

            <div className="space-y-1 py-4">
              {steps.map((step) => (
                <div key={step.id}>
                  <StepRow step={step} />
                  {step.id === "downloading" &&
                    step.status === "in_progress" &&
                    downloadProgress && (
                      <div className="ml-7 mt-0.5 mb-1">
                        <div className="h-1 w-full rounded-full bg-muted overflow-hidden">
                          <div
                            className={cn(
                              "h-full rounded-full bg-foreground/60 transition-all duration-300",
                              !downloadProgress.total && "animate-pulse",
                            )}
                            style={
                              downloadProgress.total
                                ? {
                                    width: `${Math.min(
                                      100,
                                      (downloadProgress.downloaded / downloadProgress.total) * 100,
                                    )}%`,
                                  }
                                : undefined
                            }
                          />
                        </div>
                        <p className="text-[10px] text-muted-foreground mt-0.5">
                          {formatBytes(downloadProgress.downloaded)}
                          {downloadProgress.total
                            ? ` / ${formatBytes(downloadProgress.total)}`
                            : ""}
                        </p>
                      </div>
                    )}
                </div>
              ))}
            </div>

            {error && (
              <div className="flex items-start gap-2 p-3 rounded-md bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800">
                <AlertTriangle className="h-4 w-4 text-red-600 dark:text-red-400 shrink-0 mt-0.5" />
                <p className="text-sm text-red-800 dark:text-red-200">{error}</p>
              </div>
            )}

            {hasFailed ? (
              <Button onClick={() => window.close()} className="w-full" size="lg">
                Close
              </Button>
            ) : (
              <Button onClick={handleRestart} disabled={!isReady} className="w-full" size="lg">
                {isReady ? "Restart Now" : "Preparing..."}
              </Button>
            )}
          </>
        )}
      </div>
    </div>
  );
}
