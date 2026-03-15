import {
  ArrowDownToLine,
  ChevronsRight,
  Info,
  Play,
  Plus,
  RotateCcw,
  Save,
  Square,
} from "lucide-react";
import { type ReactNode, useCallback, useEffect, useState } from "react";
import { Button, type ButtonProps } from "@/components/ui/button";
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

function ToolbarButton({
  className,
  variant = "ghost",
  ...props
}: ButtonProps) {
  return (
    <Button
      variant={variant}
      size="sm"
      className={cn(
        "h-8 gap-1.5 px-2 text-xs font-medium text-foreground whitespace-nowrap shadow-none hover:bg-muted/80",
        className,
      )}
      {...props}
    />
  );
}

function ToolbarGroup({
  children,
  className,
}: {
  children: ReactNode;
  className?: string;
}) {
  return (
    <div className={cn("flex min-w-0 flex-wrap items-center gap-1", className)}>
      {children}
    </div>
  );
}

function ToolbarSeparator() {
  return (
    <div
      aria-hidden="true"
      className="hidden h-4 w-px shrink-0 bg-border md:block"
    />
  );
}

interface NotebookToolbarProps {
  kernelStatus: KernelStatus;
  kernelErrorMessage?: string | null;
  envSource: string | null;
  /** Pre-start hint: "uv" | "conda" | "pixi" | null, derived from notebook metadata */
  envTypeHint?: EnvBadgeVariant | null;
  dirty: boolean;
  envProgress: EnvProgressState | null;
  runtime?: string;
  onSave: () => void;
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
  kernelErrorMessage,
  envSource,
  envTypeHint,
  dirty,
  envProgress,
  runtime = "python",
  onSave,
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
  const kernelStatusText = getKernelStatusLabel(kernelStatus);
  const isKernelNotStarted =
    kernelStatus === KERNEL_STATUS.NOT_STARTED ||
    kernelStatus === KERNEL_STATUS.SHUTDOWN;
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
        ? envSource.startsWith("conda:pixi")
          ? "pixi"
          : envSource.startsWith("conda")
            ? "conda"
            : "uv"
        : (envTypeHint ?? null)
      : null;

  return (
    <header
      data-testid="notebook-toolbar"
      className="sticky top-0 z-10 border-b bg-background/95 backdrop-blur supports-backdrop-filter:bg-background/60 select-none"
    >
      <div className="flex min-h-10 flex-wrap items-center gap-x-3 gap-y-1.5 px-3 py-2">
        <ToolbarGroup className="flex-1">
          <ToolbarButton
            type="button"
            onClick={onSave}
            className={cn(dirty ? "text-foreground" : "text-foreground/75")}
            title="Save (Cmd+S)"
            data-testid="save-button"
          >
            <Save className="h-3.5 w-3.5" />
            <span>Save</span>
            {dirty && <span className="text-[10px]">&bull;</span>}
          </ToolbarButton>

          <ToolbarSeparator />

          <ToolbarButton
            type="button"
            onClick={() => onAddCell("code", focusedCellId ?? lastCellId)}
            className="text-foreground/80"
            title="Add code cell"
            data-testid="add-code-cell-button"
          >
            <Plus className="h-3 w-3" />
            <span>Code</span>
          </ToolbarButton>
          <ToolbarButton
            type="button"
            onClick={() => onAddCell("markdown", focusedCellId ?? lastCellId)}
            className="text-foreground/80"
            title="Add markdown cell"
            data-testid="add-markdown-cell-button"
          >
            <Plus className="h-3 w-3" />
            <span>Markdown</span>
          </ToolbarButton>

          <ToolbarSeparator />

          {!isKernelRunning && (
            <ToolbarButton
              type="button"
              onClick={handleStartKernel}
              disabled={listKernelspecs && kernelspecs.length === 0}
              className="text-foreground/80"
              title="Start kernel"
              data-testid="start-kernel-button"
            >
              <Play className="h-3 w-3" fill="currentColor" />
              <span>Start Kernel</span>
            </ToolbarButton>
          )}

          <ToolbarButton
            type="button"
            onClick={onRunAllCells}
            title="Run all cells"
            data-testid="run-all-button"
          >
            <ChevronsRight className="h-3.5 w-3.5" />
            <span>Run All</span>
          </ToolbarButton>

          <ToolbarButton
            type="button"
            onClick={onRestartKernel}
            title="Restart kernel"
            data-testid="restart-kernel-button"
          >
            <RotateCcw className="h-3 w-3" />
            <span>Restart</span>
          </ToolbarButton>

          <ToolbarButton
            type="button"
            onClick={onRestartAndRunAll}
            title="Restart kernel and run all cells"
            aria-label="Restart kernel and run all cells"
            data-testid="restart-run-all-button"
          >
            <RotateCcw className="h-3 w-3" />
            <ChevronsRight className="h-3 w-3 -ml-1" />
            <span className="hidden lg:inline">Restart &amp; Run All</span>
          </ToolbarButton>

          {isKernelRunning && (
            <ToolbarButton
              type="button"
              onClick={onInterruptKernel}
              className={
                kernelStatus === KERNEL_STATUS.BUSY
                  ? "text-destructive hover:bg-destructive/10"
                  : "text-foreground"
              }
              title="Interrupt kernel"
              data-testid="interrupt-kernel-button"
            >
              <Square
                className="h-3 w-3"
                fill={
                  kernelStatus === KERNEL_STATUS.BUSY ? "currentColor" : "none"
                }
              />
              <span>Interrupt</span>
            </ToolbarButton>
          )}
        </ToolbarGroup>

        <ToolbarGroup className="basis-full pt-0.5 sm:ml-auto sm:basis-auto sm:justify-end sm:pt-0">
          {updateStatus === "available" && onRestartToUpdate && (
            <ToolbarButton
              type="button"
              variant="outline"
              onClick={onRestartToUpdate}
              data-testid="update-download-button"
              className="h-7 rounded-full border-violet-500/30 bg-violet-500/10 px-2 text-[10px] font-semibold text-violet-700 hover:bg-violet-500/20 hover:text-violet-800 dark:text-violet-300 dark:hover:text-violet-200"
              title={`Prepare to update to v${updateVersion}`}
            >
              <ArrowDownToLine className="h-3 w-3" />
              <span>Update {updateVersion}</span>
            </ToolbarButton>
          )}

          <ToolbarButton
            type="button"
            variant="outline"
            onClick={onToggleDependencies}
            data-testid="deps-toggle"
            data-runtime={runtime}
            className={cn(
              "h-7 rounded-full px-2 text-[10px] font-semibold",
              runtime === "deno"
                ? "border-emerald-500/30 bg-emerald-500/10 text-emerald-700 hover:bg-emerald-500/20 hover:text-emerald-800 dark:text-emerald-300 dark:hover:text-emerald-200"
                : "border-blue-500/30 bg-blue-500/10 text-blue-700 hover:bg-blue-500/20 hover:text-blue-800 dark:text-blue-300 dark:hover:text-blue-200",
              isDepsOpen && "ring-2 ring-current/20",
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
                <span className="opacity-50">·</span>
                {envManager === "uv" && (
                  <UvIcon className="h-2.5 w-2.5 text-fuchsia-700 dark:text-fuchsia-300" />
                )}
                {envManager === "conda" && (
                  <CondaIcon className="h-2.5 w-2.5 text-emerald-700 dark:text-emerald-300" />
                )}
                {envManager === "pixi" && (
                  <PixiIcon className="h-2.5 w-2.5 text-amber-700 dark:text-amber-300" />
                )}
              </>
            )}
          </ToolbarButton>

          <div
            className="flex items-center gap-1.5 whitespace-nowrap rounded-full border border-border/70 bg-muted/35 px-2 py-1"
            role="status"
            aria-label={`Kernel: ${kernelStatusDescription}`}
            title={envErrorMessage ? undefined : kernelStatusTooltip}
          >
            <div
              className={cn(
                "h-2 w-2 shrink-0 rounded-full",
                kernelStatus === KERNEL_STATUS.IDLE && "bg-green-500",
                kernelStatus === KERNEL_STATUS.BUSY && "bg-amber-500",
                kernelStatus === KERNEL_STATUS.STARTING &&
                  "bg-blue-500 animate-pulse",
                isKernelNotStarted && "bg-gray-400 dark:bg-gray-500",
                (kernelStatus === KERNEL_STATUS.ERROR || envErrorMessage) &&
                  "bg-red-500",
              )}
            />
            <span className="text-xs text-foreground/80 whitespace-nowrap">
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
        </ToolbarGroup>
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
    </header>
  );
}
