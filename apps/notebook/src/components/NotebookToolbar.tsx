import {
  ArrowDownToLine,
  ChevronsRight,
  Code,
  Info,
  LetterText,
  Play,
  RotateCcw,
  Square,
} from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import {
  HoverCard,
  HoverCardContent,
  HoverCardTrigger,
} from "@/components/ui/hover-card";
import { cn } from "@/lib/utils";
import type { EnvProgressState } from "../hooks/useEnvProgress";
import type { UpdateStatus } from "../hooks/useUpdater";
import {
  getKernelStatusLabel,
  KERNEL_STATUS,
  type KernelStatus,
} from "../lib/kernel-status";
import type { KernelspecInfo } from "../types";
import { CondaIcon, DenoIcon, PixiIcon, PythonIcon, UvIcon } from "./icons";

/** Badge color variant for environment sources */
type EnvBadgeVariant = "uv" | "conda" | "pixi";

interface NotebookToolbarProps {
  kernelStatus: KernelStatus;
  startingPhase?: string;
  kernelErrorMessage?: string | null;
  envSource: string | null;
  /** Pre-start hint: "uv" | "conda" | "pixi" | null, derived from notebook metadata */
  envTypeHint?: EnvBadgeVariant | null;
  envProgress: EnvProgressState | null;
  runtime?: string | null;
  onStartKernel: (name: string) => void;
  onInterruptKernel: () => void;
  onRestartKernel: () => void;
  onRunAllCells: () => void;
  onRestartAndRunAll: () => void;
  focusedCellId?: string | null;
  lastCellId?: string | null;
  onAddCell: (type: "code" | "markdown", afterCellId?: string | null) => void;
  onToggleDependencies: () => void;
  isDepsOpen?: boolean;
  listKernelspecs?: () => Promise<KernelspecInfo[]>;
  updateStatus?: UpdateStatus;
  updateVersion?: string | null;
  onRestartToUpdate?: () => void;
}

export function NotebookToolbar({
  kernelStatus,
  startingPhase,
  kernelErrorMessage,
  envSource,
  envTypeHint,
  envProgress,
  runtime = null,
  onStartKernel,
  onInterruptKernel,
  onRestartKernel,
  onRunAllCells,
  onRestartAndRunAll,
  focusedCellId,
  lastCellId,
  onAddCell,
  onToggleDependencies,
  isDepsOpen = false,
  listKernelspecs,
  updateStatus,
  updateVersion,
  onRestartToUpdate,
}: NotebookToolbarProps) {
  const [kernelspecs, setKernelspecs] = useState<KernelspecInfo[]>([]);

  useEffect(() => {
    if (listKernelspecs) {
      listKernelspecs().then(setKernelspecs);
    }
  }, [listKernelspecs]);

  const handleStartKernel = useCallback(() => {
    // In daemon mode (no listKernelspecs), just call with empty name - backend auto-selects
    if (!listKernelspecs) {
      onStartKernel("");
      return;
    }
    // Default to python3 or first available
    const python = kernelspecs.find(
      (k) => k.name === "python3" || k.name === "python",
    );
    const spec = python ?? kernelspecs[0];
    if (spec) {
      onStartKernel(spec.name);
    }
  }, [kernelspecs, onStartKernel, listKernelspecs]);

  const isKernelRunning =
    kernelStatus === KERNEL_STATUS.IDLE ||
    kernelStatus === KERNEL_STATUS.BUSY ||
    kernelStatus === KERNEL_STATUS.STARTING;
  const kernelStatusText = getKernelStatusLabel(kernelStatus, startingPhase);
  const envErrorMessage = envProgress?.error ?? null;
  const envStatusText = envProgress?.statusText ?? kernelStatusText;
  const kernelStatusDescription = envProgress?.isActive
    ? envStatusText
    : envErrorMessage
      ? envStatusText
      : kernelStatus === KERNEL_STATUS.ERROR && kernelErrorMessage
        ? `Error \u2014 ${kernelErrorMessage}`
        : kernelStatusText;
  const kernelStatusTooltip = envProgress?.isActive
    ? envStatusText
    : kernelStatus === KERNEL_STATUS.ERROR && kernelErrorMessage
      ? `Error \u2014 ${kernelErrorMessage}`
      : kernelStatusText;

  // Derive env manager label for the runtime pill (e.g. "uv", "conda", "pixi")
  const envManager: EnvBadgeVariant | null =
    runtime === "python"
      ? envSource &&
        (kernelStatus === KERNEL_STATUS.IDLE ||
          kernelStatus === KERNEL_STATUS.BUSY)
        ? envSource.startsWith("pixi:")
          ? "pixi"
          : envSource.startsWith("conda")
            ? "conda"
            : "uv"
        : (envTypeHint ?? null)
      : null;

  return (
    <header
      data-testid="notebook-toolbar"
      className="@container sticky top-0 z-10 border-b bg-background/95 backdrop-blur supports-backdrop-filter:bg-background/60 select-none"
    >
      <div className="flex h-10 items-center gap-2 px-3">
        {/* Add cells */}
        <button
          type="button"
          onClick={() => onAddCell("code", focusedCellId ?? lastCellId)}
          className="flex items-center gap-1 whitespace-nowrap rounded px-2 py-1 text-xs text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
          title="Add code cell"
          aria-label="Add code cell"
          data-testid="add-code-cell-button"
        >
          <Code className="h-3 w-3" />
          <span className="hidden @[40rem]:inline">Code</span>
        </button>
        <button
          type="button"
          onClick={() => onAddCell("markdown", focusedCellId ?? lastCellId)}
          className="flex items-center gap-1 whitespace-nowrap rounded px-2 py-1 text-xs text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
          title="Add markdown cell"
          aria-label="Add markdown cell"
          data-testid="add-markdown-cell-button"
        >
          <LetterText className="h-3 w-3" />
          <span className="hidden @[40rem]:inline">Markdown</span>
        </button>

        <div className="h-4 w-px bg-border" />

        {/* Kernel controls */}
        {!isKernelRunning && (
          <button
            type="button"
            onClick={handleStartKernel}
            disabled={listKernelspecs && kernelspecs.length === 0}
            className="flex items-center gap-1 whitespace-nowrap rounded px-2 py-1 text-xs text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:opacity-50"
            title="Start kernel"
            aria-label="Start kernel"
            data-testid="start-kernel-button"
          >
            <Play className="h-3 w-3" fill="currentColor" />
            <span className="hidden @[40rem]:inline">Start Kernel</span>
          </button>
        )}
        <button
          type="button"
          onClick={onRunAllCells}
          className="flex items-center gap-1 whitespace-nowrap rounded px-2 py-1 text-xs text-foreground transition-colors hover:bg-muted"
          title="Run all cells"
          aria-label="Run all cells"
          data-testid="run-all-button"
        >
          <ChevronsRight className="h-3.5 w-3.5" />
          <span className="hidden @[40rem]:inline">Run All</span>
        </button>
        <button
          type="button"
          onClick={onRestartKernel}
          className="flex items-center gap-1 whitespace-nowrap rounded px-2 py-1 text-xs text-foreground transition-colors hover:bg-muted"
          title="Restart kernel"
          aria-label="Restart kernel"
          data-testid="restart-kernel-button"
        >
          <RotateCcw className="h-3 w-3" />
          <span className="hidden @[40rem]:inline">Restart</span>
        </button>
        <button
          type="button"
          onClick={onRestartAndRunAll}
          className="flex items-center gap-1 rounded px-2 py-1 text-xs text-foreground transition-colors hover:bg-muted"
          title="Restart kernel and run all cells"
          data-testid="restart-run-all-button"
        >
          <RotateCcw className="h-3 w-3" />
          <ChevronsRight className="h-3 w-3 -ml-1" />
        </button>
        {isKernelRunning && (
          <button
            type="button"
            onClick={onInterruptKernel}
            className={cn(
              "flex items-center gap-1 whitespace-nowrap rounded px-2 py-1 text-xs transition-colors",
              kernelStatus === KERNEL_STATUS.BUSY
                ? "text-destructive hover:bg-destructive/10"
                : "text-foreground hover:bg-muted",
            )}
            title="Interrupt kernel"
            aria-label="Interrupt kernel"
            data-testid="interrupt-kernel-button"
          >
            <Square
              className="h-3 w-3"
              fill={
                kernelStatus === KERNEL_STATUS.BUSY ? "currentColor" : "none"
              }
            />
            <span className="hidden @[40rem]:inline">Interrupt</span>
          </button>
        )}

        <div className="flex-1" />

        {/* Update available */}
        {updateStatus === "available" && onRestartToUpdate && (
          <button
            type="button"
            onClick={onRestartToUpdate}
            data-testid="update-download-button"
            className="flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-medium bg-violet-500/10 text-violet-600 hover:bg-violet-500/20 dark:text-violet-400 transition-colors"
            title={`Prepare to update to v${updateVersion}`}
          >
            <ArrowDownToLine className="h-3 w-3" />
            <span>Update {updateVersion}</span>
          </button>
        )}

        {/* Runtime / deps toggle — hidden until runtime is known to avoid flicker */}
        {runtime && (
          <button
            type="button"
            onClick={onToggleDependencies}
            data-testid="deps-toggle"
            data-runtime={runtime}
            data-env-manager={envManager || undefined}
            className={cn(
              "flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-medium transition-colors",
              runtime === "deno"
                ? "bg-emerald-500/10 text-emerald-600 hover:bg-emerald-500/20 dark:text-emerald-400"
                : "bg-blue-500/10 text-blue-600 hover:bg-blue-500/20 dark:text-blue-400",
              isDepsOpen && "ring-1 ring-current/25",
            )}
            title={(() => {
              const lang = runtime === "deno" ? "Deno/TypeScript" : "Python";
              const mgr = envManager ? ` · ${envManager}` : "";
              const action = isDepsOpen
                ? "close environment panel"
                : "open environment panel";
              return `${lang}${mgr} — ${action}`;
            })()}
          >
            {runtime === "deno" ? (
              <>
                <DenoIcon className="h-3 w-3" />
                <span>Deno</span>
              </>
            ) : (
              <>
                <PythonIcon className="h-3 w-3" />
                <span>Python</span>
              </>
            )}
            {envManager && (
              <>
                <span className="opacity-40">·</span>
                {envManager === "uv" && (
                  <UvIcon className="h-2 w-2 text-fuchsia-600 dark:text-fuchsia-400" />
                )}
                {envManager === "conda" && (
                  <CondaIcon className="h-2.5 w-2.5 text-emerald-600 dark:text-emerald-400" />
                )}
                {envManager === "pixi" && (
                  <PixiIcon className="h-2.5 w-2.5 text-amber-600 dark:text-amber-400" />
                )}
              </>
            )}
          </button>
        )}

        {/* Kernel status */}
        <div
          className="flex items-center gap-1.5 whitespace-nowrap"
          role="status"
          aria-label={`Kernel: ${kernelStatusDescription}`}
          title={envErrorMessage ? undefined : kernelStatusTooltip}
          data-testid="kernel-status"
          data-kernel-status={kernelStatus}
        >
          <div
            className={cn(
              "h-2 w-2 shrink-0 rounded-full",
              kernelStatus === KERNEL_STATUS.IDLE && "bg-green-500",
              kernelStatus === KERNEL_STATUS.BUSY && "bg-amber-500",
              kernelStatus === KERNEL_STATUS.STARTING &&
                "bg-blue-500 animate-pulse",
              kernelStatus === KERNEL_STATUS.NOT_STARTED &&
                "bg-blue-500 animate-pulse",
              kernelStatus === KERNEL_STATUS.SHUTDOWN &&
                "bg-gray-400 dark:bg-gray-500",
              (kernelStatus === KERNEL_STATUS.ERROR || envErrorMessage) &&
                "bg-red-500",
            )}
          />
          <span className="text-xs text-muted-foreground whitespace-nowrap">
            {envProgress?.isActive ? (
              envStatusText
            ) : envErrorMessage ? (
              <HoverCard openDelay={150} closeDelay={100}>
                <HoverCardTrigger asChild>
                  <span className="cursor-help text-red-600 underline decoration-dotted underline-offset-2 dark:text-red-400">
                    {envStatusText}
                  </span>
                </HoverCardTrigger>
                <HoverCardContent
                  align="end"
                  className="w-80 max-w-[calc(100vw-2rem)] p-3"
                >
                  <div className="space-y-1">
                    <p className="text-xs font-medium text-red-600 dark:text-red-400">
                      Environment error
                    </p>
                    <pre className="whitespace-pre-wrap break-words font-mono text-[11px] leading-relaxed text-muted-foreground">
                      {envErrorMessage}
                    </pre>
                  </div>
                </HoverCardContent>
              </HoverCard>
            ) : (
              <span
                className={cn(
                  "capitalize",
                  kernelStatus === KERNEL_STATUS.ERROR &&
                    "text-red-600 dark:text-red-400",
                )}
              >
                {kernelStatusText}
              </span>
            )}
          </span>
        </div>
      </div>

      {/* Deno install prompt */}
      {runtime === "deno" &&
        kernelStatus === KERNEL_STATUS.ERROR &&
        kernelErrorMessage && (
          <div className="border-t px-3 py-2">
            <div className="flex items-start gap-2 text-xs text-amber-700 dark:text-amber-400">
              <Info className="h-3.5 w-3.5 mt-0.5 shrink-0" />
              <span>
                <span className="font-medium">Deno not available.</span>{" "}
                Auto-install failed. Install manually with{" "}
                <code className="rounded bg-amber-500/20 px-1">
                  curl -fsSL https://deno.land/install.sh | sh
                </code>{" "}
                and restart.
              </span>
            </div>
          </div>
        )}
      {/* Pixi ipykernel install prompt — only when daemon signals missing_ipykernel */}
      {runtime === "python" &&
        kernelStatus === KERNEL_STATUS.ERROR &&
        envSource?.startsWith("pixi:") &&
        startingPhase === "missing_ipykernel" && (
          <div className="border-t px-3 py-2">
            <div className="flex items-start gap-2 text-xs text-amber-700 dark:text-amber-400">
              <Info className="h-3.5 w-3.5 mt-0.5 shrink-0" />
              <span>
                <span className="font-medium">
                  ipykernel not found in pixi.toml.
                </span>{" "}
                Run{" "}
                <code className="rounded bg-amber-500/20 px-1">
                  pixi add ipykernel
                </code>{" "}
                in your project directory and restart.
              </span>
            </div>
          </div>
        )}
    </header>
  );
}
