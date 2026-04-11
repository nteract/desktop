import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { AlertTriangle, ArrowLeft, Check, Loader2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import { cn } from "@/lib/utils";
import { CondaIcon, DenoIcon, PixiIcon, PythonIcon, UvIcon } from "../src/components/icons";
import type { DaemonStatus } from "./types";

type Runtime = "python" | "deno";
type PythonEnv = "uv" | "conda" | "pixi";

type SetupStep = {
  id: string;
  label: string;
  status: "pending" | "in_progress" | "completed" | "failed";
  error?: string;
};

interface SelectionCardProps {
  selected: boolean;
  onClick: () => void;
  icon: React.ComponentType<{ className?: string }>;
  title: string;
  subtitle?: string;
  description: string;
  colorClass: {
    bg: string;
    text: string;
    ring: string;
    iconBg: string;
  };
}

function SelectionCard({
  selected,
  onClick,
  icon: Icon,
  title,
  subtitle,
  description,
  colorClass,
}: SelectionCardProps) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "relative flex flex-col items-center justify-center gap-4 rounded-2xl border-2 p-8 w-52 h-64",
        "transition-all duration-200 ease-out cursor-pointer",
        "hover:scale-[1.02] hover:shadow-lg",
        selected
          ? [
              "scale-[1.02] shadow-lg",
              colorClass.bg,
              colorClass.ring,
              "ring-2 ring-offset-2 ring-offset-background",
              "border-transparent",
            ]
          : ["border-border/50 hover:border-border bg-card"],
      )}
    >
      <div
        className={cn(
          "flex items-center justify-center rounded-2xl p-4",
          selected ? colorClass.iconBg : "bg-muted",
        )}
      >
        <Icon
          className={cn(
            "h-16 w-16 transition-colors",
            selected ? colorClass.text : "text-muted-foreground",
          )}
        />
      </div>
      <div className="text-center space-y-1">
        <h3 className={cn("text-lg font-semibold", selected ? colorClass.text : "text-foreground")}>
          {title}
        </h3>
        {subtitle && (
          <p
            className={cn(
              "text-xs font-medium",
              selected ? colorClass.text : "text-muted-foreground",
            )}
          >
            {subtitle}
          </p>
        )}
        <p className="text-xs text-muted-foreground">{description}</p>
      </div>
      {selected && (
        <div className={cn("absolute top-3 right-3 rounded-full p-1", colorClass.iconBg)}>
          <Check className={cn("h-4 w-4", colorClass.text)} />
        </div>
      )}
    </button>
  );
}

function PageDots({ current, total }: { current: number; total: number }) {
  return (
    <div className="flex items-center gap-2">
      {Array.from({ length: total }, (_, i) => (
        <div
          key={i}
          className={cn(
            "h-2 w-2 rounded-full transition-colors",
            i + 1 === current ? "bg-foreground" : "bg-muted-foreground/30",
          )}
        />
      ))}
    </div>
  );
}

const BRAND_COLORS = {
  python: {
    bg: "bg-blue-500/10",
    text: "text-blue-600 dark:text-blue-400",
    ring: "ring-blue-500",
    iconBg: "bg-blue-500/20",
  },
  deno: {
    bg: "bg-emerald-500/10",
    text: "text-emerald-600 dark:text-emerald-400",
    ring: "ring-emerald-500",
    iconBg: "bg-emerald-500/20",
  },
  uv: {
    bg: "bg-fuchsia-500/10",
    text: "text-fuchsia-600 dark:text-fuchsia-400",
    ring: "ring-fuchsia-500",
    iconBg: "bg-fuchsia-500/20",
  },
  conda: {
    bg: "bg-green-500/10",
    text: "text-green-600 dark:text-green-400",
    ring: "ring-green-500",
    iconBg: "bg-green-500/20",
  },
  pixi: {
    bg: "bg-amber-500/10",
    text: "text-amber-600 dark:text-amber-400",
    ring: "ring-amber-500",
    iconBg: "bg-amber-500/20",
  },
};

/**
 * First-launch onboarding screen with paged wizard.
 *
 * Page 1: Runtime selection (Python vs Deno)
 * Page 2: Python environment manager (UV vs Conda)
 *
 * Daemon installation runs in background throughout.
 */
export default function App() {
  const [page, setPage] = useState<1 | 2>(1);
  const [runtime, setRuntime] = useState<Runtime | null>(null);
  const [pythonEnv, setPythonEnv] = useState<PythonEnv | null>(null);
  const [steps, setSteps] = useState<SetupStep[]>([
    { id: "daemon", label: "Installing runtime daemon", status: "in_progress" },
    { id: "tools", label: "Preparing environments", status: "pending" },
  ]);
  const [daemonReady, setDaemonReady] = useState(false);
  const [daemonFailed, setDaemonFailed] = useState(false);
  const [poolReady, setPoolReady] = useState(false);
  const [setupComplete, setSetupComplete] = useState(false);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);

  // Listen for daemon progress events
  useEffect(() => {
    const handleStatus = (status: DaemonStatus) => {
      if (!status) return;

      if (status.status === "ready") {
        setDaemonReady(true);
        setDaemonFailed(false);
        setSteps((prev) =>
          prev.map((s) => (s.id === "daemon" ? { ...s, status: "completed" } : s)),
        );
        setErrorMessage(null);
      } else if (status.status === "failed") {
        setDaemonFailed(true);
        setSteps((prev) =>
          prev.map((s) =>
            s.id === "daemon" ? { ...s, status: "failed", error: status.error } : s,
          ),
        );
        setErrorMessage(status.guidance || status.error);
      } else if (
        status.status === "checking" ||
        status.status === "installing" ||
        status.status === "starting" ||
        status.status === "waiting_for_ready"
      ) {
        setSteps((prev) =>
          prev.map((s) => (s.id === "daemon" ? { ...s, status: "in_progress" } : s)),
        );
      }
    };

    // Check current daemon status on mount
    invoke<DaemonStatus | null>("get_daemon_status")
      .then((status) => {
        if (status) handleStatus(status);
      })
      .catch(() => {});

    const unlistenProgress = listen<DaemonStatus>("daemon:progress", (event) =>
      handleStatus(event.payload),
    );

    return () => {
      unlistenProgress.then((unlisten) => unlisten()).catch(() => {});
    };
  }, []);

  // Poll for pool readiness once daemon is ready
  useEffect(() => {
    if (!daemonReady || poolReady) return;

    setSteps((prev) => prev.map((s) => (s.id === "tools" ? { ...s, status: "in_progress" } : s)));

    let cancelled = false;
    const pollPool = async () => {
      while (!cancelled) {
        try {
          const state = await invoke<{
            uv: { available: number };
            conda: { available: number };
          }>("get_pool_status");

          if (state.uv.available > 0 || state.conda.available > 0) {
            setPoolReady(true);
            setSteps((prev) =>
              prev.map((s) => (s.id === "tools" ? { ...s, status: "completed" } : s)),
            );
            return;
          }
        } catch {
          // Pool not ready yet
        }
        await new Promise((resolve) => setTimeout(resolve, 1000));
      }
    };

    pollPool();
    return () => {
      cancelled = true;
    };
  }, [daemonReady, poolReady]);

  // Handle runtime selection
  const handleRuntimeSelect = useCallback((selected: Runtime) => {
    setRuntime(selected);
  }, []);

  // Advance to page 2
  const handleNext = useCallback(() => {
    if (runtime) {
      setPage(2);
    }
  }, [runtime]);

  // Handle Python env selection with auto-advance to ready state
  const handlePythonEnvSelect = useCallback((selected: PythonEnv) => {
    setPythonEnv(selected);
  }, []);

  // Go back to page 1
  const handleBack = useCallback(() => {
    setPage(1);
  }, []);

  // Save settings and complete onboarding
  const handleGetStarted = useCallback(async () => {
    if (!runtime || !pythonEnv) return;
    if (!daemonReady || !poolReady) return;

    try {
      // Save settings to daemon
      await invoke("set_synced_setting", {
        key: "default_runtime",
        value: runtime,
      });
      await invoke("set_synced_setting", {
        key: "default_python_env",
        value: pythonEnv,
      });
      await invoke("set_synced_setting", {
        key: "onboarding_completed",
        value: true,
      });

      // Show completing state while we create the notebook window
      setSetupComplete(true);

      // Pass selected values directly to avoid settings race condition
      // Await onComplete so we can handle failures properly
      try {
        await invoke("complete_onboarding", {
          defaultRuntime: runtime,
          defaultPythonEnv: pythonEnv,
        });
        // Window closes itself on success - no further action needed
      } catch (completeError) {
        // onComplete failed - reset state so user can retry
        console.error("Failed to complete onboarding:", completeError);
        setSetupComplete(false);
        setErrorMessage("Failed to create notebook window. Please try again.");
      }
    } catch (e) {
      console.error("Failed to save onboarding settings:", e);
      setErrorMessage("Failed to save settings. Please try again.");
    }
  }, [daemonReady, poolReady, runtime, pythonEnv]);

  // Skip onboarding when daemon failed - use current selections or defaults
  const handleSkip = useCallback(async () => {
    await invoke("complete_onboarding", {
      defaultRuntime: runtime ?? "python",
      defaultPythonEnv: pythonEnv ?? "uv",
    });
  }, [runtime, pythonEnv]);

  const completedSteps = steps.filter((s) => s.status === "completed").length;
  const totalSteps = steps.length;
  const progressPercent = (completedSteps / totalSteps) * 100;

  const canProceed =
    page === 2 &&
    runtime !== null &&
    pythonEnv !== null &&
    daemonReady &&
    poolReady &&
    !setupComplete;

  // Page titles based on selections
  const page2Title = runtime === "deno" ? "Ok but if you did use Python..." : "Python Environment";
  const page2Subtitle =
    runtime === "deno" ? "Which package manager would you use?" : "Choose your package manager";

  return (
    <div className="flex min-h-screen flex-col items-center justify-center bg-background p-8">
      <div className="w-full max-w-xl space-y-8">
        {/* Page 1: Runtime Selection */}
        {page === 1 && (
          <>
            <div className="text-center space-y-2">
              <h1 className="text-3xl font-semibold tracking-tight">Welcome to nteract!</h1>
              <p className="text-muted-foreground">Choose your preferred notebook runtime</p>
            </div>

            <div className="flex items-center justify-center gap-6">
              <SelectionCard
                selected={runtime === "python"}
                onClick={() => handleRuntimeSelect("python")}
                icon={PythonIcon}
                title="Python"
                description="Scientific computing & data science"
                colorClass={BRAND_COLORS.python}
              />
              <SelectionCard
                selected={runtime === "deno"}
                onClick={() => handleRuntimeSelect("deno")}
                icon={DenoIcon}
                title="Deno"
                description="TypeScript/JS notebooks"
                colorClass={BRAND_COLORS.deno}
              />
            </div>

            <div className="flex justify-center">
              <PageDots current={1} total={2} />
            </div>

            {/* Next button */}
            <Button onClick={handleNext} disabled={runtime === null} className="w-full" size="lg">
              {runtime === null ? "Select a runtime" : "Next"}
            </Button>
          </>
        )}

        {/* Page 2: Python Environment Manager */}
        {page === 2 && (
          <>
            <div className="text-center space-y-2">
              <h1 className="text-3xl font-semibold tracking-tight">{page2Title}</h1>
              <p className="text-muted-foreground">{page2Subtitle}</p>
            </div>

            <div className="flex items-center justify-center gap-4">
              <SelectionCard
                selected={pythonEnv === "uv"}
                onClick={() => handlePythonEnvSelect("uv")}
                icon={UvIcon}
                title="UV"
                description="PyPI & pip-compatible"
                colorClass={BRAND_COLORS.uv}
              />
              <SelectionCard
                selected={pythonEnv === "conda"}
                onClick={() => handlePythonEnvSelect("conda")}
                icon={CondaIcon}
                title="Conda"
                description="Scientific stack & private channels"
                colorClass={BRAND_COLORS.conda}
              />
              <SelectionCard
                selected={pythonEnv === "pixi"}
                onClick={() => handlePythonEnvSelect("pixi")}
                icon={PixiIcon}
                title="Pixi"
                description="Conda + pip unified"
                colorClass={BRAND_COLORS.pixi}
              />
            </div>

            <div className="flex items-center justify-between">
              <Button variant="ghost" size="sm" onClick={handleBack} className="gap-1">
                <ArrowLeft className="h-4 w-4" />
                Back
              </Button>
              <PageDots current={2} total={2} />
              <div className="w-[60px]" /> {/* Spacer for centering */}
            </div>

            {/* Get Started button */}
            <Button onClick={handleGetStarted} disabled={!canProceed} className="w-full" size="lg">
              {setupComplete
                ? "All set!"
                : canProceed
                  ? "Get Started"
                  : pythonEnv === null
                    ? "Select a package manager"
                    : "Setting up..."}
            </Button>

            {/* Continue anyway button when daemon fails */}
            {daemonFailed && !setupComplete && (
              <Button onClick={handleSkip} variant="ghost" className="w-full" size="sm">
                Continue anyway
              </Button>
            )}
          </>
        )}

        {/* Error message */}
        {errorMessage && (
          <div className="flex items-start gap-2 p-3 rounded-md bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800">
            <AlertTriangle className="h-4 w-4 text-amber-600 dark:text-amber-400 shrink-0 mt-0.5" />
            <p className="text-sm text-amber-800 dark:text-amber-200">{errorMessage}</p>
          </div>
        )}

        {/* Setup progress (subtle, at bottom) */}
        <div className="space-y-2 pt-4 border-t border-border/50">
          <Progress value={progressPercent} className="h-1" />
          <div className="flex items-center justify-center gap-4 text-xs text-muted-foreground">
            {steps.map((step) => (
              <div key={step.id} className="flex items-center gap-1.5">
                {step.status === "completed" && <Check className="h-3 w-3 text-green-600" />}
                {step.status === "in_progress" && <Loader2 className="h-3 w-3 animate-spin" />}
                {step.status === "pending" && (
                  <div className="h-3 w-3 rounded-full border border-muted-foreground/30" />
                )}
                {step.status === "failed" && <AlertTriangle className="h-3 w-3 text-amber-600" />}
                <span
                  className={cn(
                    step.status === "failed" && "text-amber-600",
                    step.status === "completed" && "text-muted-foreground/70",
                  )}
                >
                  {step.label}
                </span>
              </div>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}
