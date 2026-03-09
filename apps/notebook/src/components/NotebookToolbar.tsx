import {
  AlertCircle,
  ArrowDownToLine,
  ChevronsRight,
  Info,
  Monitor,
  Moon,
  Play,
  Plus,
  RotateCcw,
  Save,
  Settings,
  Square,
  Sun,
  X,
} from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import { Slider } from "@/components/ui/slider";
import type { ThemeMode } from "@/hooks/useSyncedSettings";
import { isKnownPythonEnv, isKnownRuntime } from "@/hooks/useSyncedSettings";
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

/** Format seconds into human-readable duration */
function formatDuration(secs: number): string {
  if (secs >= 86400) {
    const days = Math.floor(secs / 86400);
    const hours = Math.floor((secs % 86400) / 3600);
    return hours > 0 ? `${days}d ${hours}h` : `${days}d`;
  }
  if (secs >= 3600) {
    const hours = Math.floor(secs / 3600);
    const mins = Math.floor((secs % 3600) / 60);
    return mins > 0 ? `${hours}h ${mins}m` : `${hours}h`;
  }
  if (secs >= 60) {
    const mins = Math.floor(secs / 60);
    const remainingSecs = secs % 60;
    return remainingSecs > 0 ? `${mins}m ${remainingSecs}s` : `${mins}m`;
  }
  return `${secs}s`;
}

// Exponential slider constants
const MIN_SECS = 5;
const MAX_SECS = 604800; // 7 days
const SLIDER_STEPS = 100;

// Convert slider position (0-100) to seconds (exponential scale)
function sliderToSeconds(position: number): number {
  // Exponential: value = MIN * (MAX/MIN)^(position/100)
  const ratio = MAX_SECS / MIN_SECS;
  const secs = Math.round(MIN_SECS * ratio ** (position / SLIDER_STEPS));
  return Math.max(MIN_SECS, Math.min(MAX_SECS, secs));
}

// Convert seconds to slider position (0-100)
function secondsToSlider(secs: number): number {
  // Inverse: position = 100 * log(value/MIN) / log(MAX/MIN)
  const ratio = MAX_SECS / MIN_SECS;
  const position = (SLIDER_STEPS * Math.log(secs / MIN_SECS)) / Math.log(ratio);
  return Math.max(0, Math.min(SLIDER_STEPS, Math.round(position)));
}

/** Keep Alive slider - exponential scale from 5s to 7 days */
function KeepAliveSlider({
  value,
  onChange,
}: {
  value: number;
  onChange: (value: number) => void;
}) {
  const [localValue, setLocalValue] = useState(value);

  // Sync local value when prop changes externally
  useEffect(() => {
    setLocalValue(value);
  }, [value]);

  const sliderPosition = secondsToSlider(localValue);

  return (
    <div className="space-y-3 pt-2 border-t border-border/50">
      <div>
        <span className="text-xs font-semibold text-muted-foreground uppercase tracking-wider">
          Advanced
        </span>
      </div>
      <div className="space-y-1">
        <div className="flex items-center justify-between">
          <span className="text-xs font-medium text-muted-foreground">
            Keep Alive
          </span>
          <span className="text-xs font-medium text-foreground tabular-nums">
            {formatDuration(localValue)}
          </span>
        </div>
        <p className="text-[10px] text-muted-foreground/70">
          Time to keep notebook runtime alive after closing
        </p>
      </div>
      <div className="py-2">
        <Slider
          value={[sliderPosition]}
          min={0}
          max={SLIDER_STEPS}
          step={1}
          onValueChange={(v) => setLocalValue(sliderToSeconds(v[0]))}
          onValueCommit={(v) => onChange(sliderToSeconds(v[0]))}
        />
      </div>
      <div className="flex justify-between text-[10px] text-muted-foreground/70">
        <span>5s</span>
        <span>7 days</span>
      </div>
    </div>
  );
}

/** Badge color variant for environment sources */
type EnvBadgeVariant = "uv" | "conda" | "pixi";

interface NotebookToolbarProps {
  kernelStatus: KernelStatus;
  kernelErrorMessage?: string | null;
  envSource: string | null;
  /** Pre-start hint: "uv" | "conda" | "pixi" | null, derived from notebook metadata */
  envTypeHint?: EnvBadgeVariant | null;
  dirty: boolean;
  hasDependencies: boolean;
  theme: ThemeMode;
  envProgress: EnvProgressState | null;
  runtime?: string;
  onThemeChange: (theme: ThemeMode) => void;
  defaultRuntime?: string;
  onDefaultRuntimeChange?: (runtime: string) => void;
  defaultPythonEnv?: string;
  onDefaultPythonEnvChange?: (env: string) => void;
  defaultUvPackages?: string[];
  onDefaultUvPackagesChange?: (packages: string[]) => void;
  defaultCondaPackages?: string[];
  onDefaultCondaPackagesChange?: (packages: string[]) => void;
  keepAliveSecs?: number;
  onKeepAliveSecsChange?: (secs: number) => void;
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
  onDownloadUpdate?: () => void;
  onRestartToUpdate?: () => void;
}

const themeOptions: { value: ThemeMode; label: string; icon: typeof Sun }[] = [
  { value: "light", label: "Light", icon: Sun },
  { value: "dark", label: "Dark", icon: Moon },
  { value: "system", label: "System", icon: Monitor },
];

/** Badge input for managing a list of package names */
function PackageBadgeInput({
  packages,
  onChange,
  placeholder,
}: {
  packages: string[];
  onChange: (packages: string[]) => void;
  placeholder?: string;
}) {
  const [inputValue, setInputValue] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  const addPackage = useCallback(
    (raw: string) => {
      const name = raw.trim();
      if (!name) return;
      if (!packages.includes(name)) {
        onChange([...packages, name]);
      }
      setInputValue("");
    },
    [packages, onChange],
  );

  const removePackage = useCallback(
    (index: number) => {
      onChange(packages.filter((_, i) => i !== index));
    },
    [packages, onChange],
  );

  return (
    <div
      className="flex flex-wrap items-center gap-1 min-h-7 max-w-md rounded-md border bg-muted/50 px-1.5 py-1 cursor-text"
      onClick={() => inputRef.current?.focus()}
    >
      {packages.map((pkg, i) => (
        <span
          key={`${pkg}-${i}`}
          className="inline-flex items-center gap-0.5 rounded-md bg-secondary text-secondary-foreground pl-1.5 pr-0.5 py-0 text-xs leading-5"
        >
          {pkg}
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              removePackage(i);
            }}
            className="rounded-sm p-0 hover:bg-muted-foreground/20"
          >
            <X className="h-3 w-3" />
          </button>
        </span>
      ))}
      <input
        ref={inputRef}
        type="text"
        value={inputValue}
        onChange={(e) => setInputValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            addPackage(inputValue);
          } else if (
            e.key === "Backspace" &&
            inputValue === "" &&
            packages.length > 0
          ) {
            removePackage(packages.length - 1);
          }
        }}
        onBlur={() => {
          if (inputValue.trim()) {
            addPackage(inputValue);
          }
        }}
        placeholder={packages.length === 0 ? placeholder : ""}
        autoComplete="off"
        autoCorrect="off"
        autoCapitalize="off"
        spellCheck={false}
        className="flex-1 min-w-[80px] bg-transparent text-xs text-foreground placeholder:text-muted-foreground focus:outline-none h-5"
      />
    </div>
  );
}

export function NotebookToolbar({
  kernelStatus,
  kernelErrorMessage,
  envSource,
  envTypeHint,
  dirty,
  theme,
  envProgress,
  runtime = "python",
  onThemeChange,
  defaultRuntime = "python",
  onDefaultRuntimeChange,
  defaultPythonEnv = "uv",
  onDefaultPythonEnvChange,
  defaultUvPackages = [],
  onDefaultUvPackagesChange,
  defaultCondaPackages = [],
  onDefaultCondaPackagesChange,
  keepAliveSecs = 30,
  onKeepAliveSecsChange,
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
  onDownloadUpdate,
  onRestartToUpdate,
}: NotebookToolbarProps) {
  const [kernelspecs, setKernelspecs] = useState<KernelspecInfo[]>([]);
  const [settingsOpen, setSettingsOpen] = useState(false);

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
    <Collapsible open={settingsOpen} onOpenChange={setSettingsOpen}>
      <header
        data-testid="notebook-toolbar"
        className="sticky top-0 z-10 border-b bg-background/95 backdrop-blur supports-backdrop-filter:bg-background/60 select-none"
      >
        <div className="flex h-10 items-center gap-2 px-3">
          {/* Save */}
          <button
            type="button"
            onClick={onSave}
            className={cn(
              "flex items-center gap-1 rounded px-2 py-1 text-xs transition-colors hover:bg-muted",
              dirty ? "text-foreground" : "text-muted-foreground",
            )}
            title="Save (Cmd+S)"
            data-testid="save-button"
          >
            <Save className="h-3.5 w-3.5" />
            {dirty && <span className="text-[10px]">&bull;</span>}
          </button>

          <div className="h-4 w-px bg-border" />

          {/* Add cells */}
          <button
            type="button"
            onClick={() => onAddCell("code", focusedCellId ?? lastCellId)}
            className="flex items-center gap-1 rounded px-2 py-1 text-xs text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
            title="Add code cell"
            data-testid="add-code-cell-button"
          >
            <Plus className="h-3 w-3" />
            Code
          </button>
          <button
            type="button"
            onClick={() => onAddCell("markdown", focusedCellId ?? lastCellId)}
            className="flex items-center gap-1 rounded px-2 py-1 text-xs text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
            title="Add markdown cell"
            data-testid="add-markdown-cell-button"
          >
            <Plus className="h-3 w-3" />
            Markdown
          </button>

          <div className="h-4 w-px bg-border" />

          {/* Kernel controls */}
          {!isKernelRunning && (
            <button
              type="button"
              onClick={handleStartKernel}
              disabled={listKernelspecs && kernelspecs.length === 0}
              className="flex items-center gap-1 rounded px-2 py-1 text-xs text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:opacity-50"
              title="Start kernel"
              data-testid="start-kernel-button"
            >
              <Play className="h-3 w-3" fill="currentColor" />
              Start Kernel
            </button>
          )}
          <button
            type="button"
            onClick={onRunAllCells}
            className="flex items-center gap-1 rounded px-2 py-1 text-xs text-foreground transition-colors hover:bg-muted"
            title="Run all cells"
            data-testid="run-all-button"
          >
            <ChevronsRight className="h-3.5 w-3.5" />
            Run All
          </button>
          <button
            type="button"
            onClick={onRestartKernel}
            className="flex items-center gap-1 rounded px-2 py-1 text-xs text-foreground transition-colors hover:bg-muted"
            title="Restart kernel"
            data-testid="restart-kernel-button"
          >
            <RotateCcw className="h-3 w-3" />
            Restart
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
                "flex items-center gap-1 rounded px-2 py-1 text-xs transition-colors",
                kernelStatus === KERNEL_STATUS.BUSY
                  ? "text-destructive hover:bg-destructive/10"
                  : "text-foreground hover:bg-muted",
              )}
              title="Interrupt kernel"
              data-testid="interrupt-kernel-button"
            >
              <Square
                className="h-3 w-3"
                fill={
                  kernelStatus === KERNEL_STATUS.BUSY ? "currentColor" : "none"
                }
              />
              Interrupt
            </button>
          )}

          <div className="flex-1" />

          {/* Update available */}
          {updateStatus === "available" && onDownloadUpdate && (
            <button
              type="button"
              onClick={onDownloadUpdate}
              data-testid="update-download-button"
              className="flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-medium bg-violet-500/10 text-violet-600 hover:bg-violet-500/20 dark:text-violet-400 transition-colors"
              title={`Download update v${updateVersion}`}
            >
              <ArrowDownToLine className="h-3 w-3" />
              <span>Update {updateVersion}</span>
            </button>
          )}
          {updateStatus === "downloading" && (
            <div
              className="flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-medium bg-violet-500/10 text-violet-500 dark:text-violet-400"
              title="Downloading update…"
            >
              <ArrowDownToLine className="h-3 w-3 animate-bounce" />
              <span>Updating…</span>
            </div>
          )}
          {updateStatus === "ready" && onRestartToUpdate && (
            <button
              type="button"
              onClick={onRestartToUpdate}
              data-testid="update-restart-button"
              className="flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-medium bg-green-500/15 text-green-600 hover:bg-green-500/25 dark:text-green-400 transition-colors"
              title={`Restart to install v${updateVersion}`}
            >
              <RotateCcw className="h-3 w-3" />
              <span>Restart to update</span>
            </button>
          )}

          {/* Runtime / deps toggle */}
          <button
            type="button"
            onClick={onToggleDependencies}
            data-testid="deps-toggle"
            data-runtime={runtime}
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

          {/* Kernel status */}
          <div
            className="flex items-center gap-1.5 whitespace-nowrap"
            role="status"
            aria-label={`Kernel: ${
              envProgress?.isActive
                ? envProgress.statusText
                : envProgress?.error
                  ? envProgress.statusText
                  : kernelStatus === KERNEL_STATUS.ERROR && kernelErrorMessage
                    ? `Error \u2014 ${kernelErrorMessage}`
                    : kernelStatusText
            }`}
            title={
              envProgress?.isActive
                ? envProgress.statusText
                : envProgress?.error
                  ? envProgress.error
                  : kernelStatus === KERNEL_STATUS.ERROR && kernelErrorMessage
                    ? `Error \u2014 ${kernelErrorMessage}`
                    : kernelStatusText
            }
          >
            <div
              className={cn(
                "h-2 w-2 shrink-0 rounded-full",
                kernelStatus === KERNEL_STATUS.IDLE && "bg-green-500",
                kernelStatus === KERNEL_STATUS.BUSY && "bg-amber-500",
                kernelStatus === KERNEL_STATUS.STARTING &&
                  "bg-blue-500 animate-pulse",
                isKernelNotStarted && "bg-gray-400 dark:bg-gray-500",
                kernelStatus === KERNEL_STATUS.ERROR && "bg-red-500",
              )}
            />
            <span className="text-xs text-muted-foreground whitespace-nowrap">
              {envProgress?.isActive ? (
                envProgress.statusText
              ) : envProgress?.error ? (
                <span className="text-red-600 dark:text-red-400">
                  {envProgress.statusText}
                </span>
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

          <div className="h-4 w-px bg-border" />

          {/* Settings gear */}
          <CollapsibleTrigger asChild>
            <button
              type="button"
              className={cn(
                "flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground",
                settingsOpen && "bg-muted text-foreground",
              )}
              aria-label="Settings"
            >
              <Settings className="h-4 w-4" />
            </button>
          </CollapsibleTrigger>
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

        {/* Collapsible settings panel */}
        <CollapsibleContent>
          <div
            className="border-t bg-background px-4 py-3 space-y-3"
            data-testid="settings-panel"
          >
            {/* Global settings */}
            <div className="flex flex-wrap items-center gap-x-6 gap-y-2">
              {/* Theme */}
              <div className="flex items-center gap-3">
                <span className="text-xs font-medium text-muted-foreground">
                  Theme
                </span>
                <div
                  className="flex items-center gap-1 rounded-md border bg-muted/50 p-0.5"
                  data-testid="settings-theme-group"
                >
                  {themeOptions.map((option) => {
                    const Icon = option.icon;
                    const isActive = theme === option.value;
                    return (
                      <button
                        key={option.value}
                        type="button"
                        onClick={() => onThemeChange(option.value)}
                        className={cn(
                          "flex items-center gap-1.5 rounded-sm px-2.5 py-1 text-xs transition-colors",
                          isActive
                            ? "bg-background text-foreground shadow-sm"
                            : "text-muted-foreground hover:text-foreground",
                        )}
                      >
                        <Icon className="h-3.5 w-3.5" />
                        {option.label}
                      </button>
                    );
                  })}
                </div>
              </div>

              {/* Default Runtime */}
              {onDefaultRuntimeChange && (
                <div className="space-y-1">
                  <div className="flex items-center gap-3">
                    <span className="text-xs font-medium text-muted-foreground">
                      Default Runtime
                    </span>
                    <div
                      className="flex items-center gap-1 rounded-md border bg-muted/50 p-0.5"
                      data-testid="settings-runtime-group"
                    >
                      <button
                        type="button"
                        onClick={() => onDefaultRuntimeChange("python")}
                        className={cn(
                          "flex items-center gap-1.5 rounded-sm px-2.5 py-1 text-xs transition-colors",
                          defaultRuntime === "python"
                            ? "bg-blue-500/15 text-blue-600 dark:text-blue-400 shadow-sm"
                            : "text-muted-foreground hover:text-foreground",
                        )}
                      >
                        <PythonIcon className="h-3.5 w-3.5" />
                        Python
                      </button>
                      <button
                        type="button"
                        onClick={() => onDefaultRuntimeChange("deno")}
                        className={cn(
                          "flex items-center gap-1.5 rounded-sm px-2.5 py-1 text-xs transition-colors",
                          defaultRuntime === "deno"
                            ? "bg-teal-500/15 text-teal-600 dark:text-teal-400 shadow-sm"
                            : "text-muted-foreground hover:text-foreground",
                        )}
                      >
                        <DenoIcon className="h-3.5 w-3.5" />
                        Deno
                      </button>
                    </div>
                  </div>
                  {defaultRuntime && !isKnownRuntime(defaultRuntime) && (
                    <div className="flex items-start gap-2 text-xs text-amber-700 dark:text-amber-400 mt-1">
                      <AlertCircle className="h-3.5 w-3.5 mt-0.5 shrink-0" />
                      <span>
                        <span className="font-medium">
                          &ldquo;{defaultRuntime}&rdquo;
                        </span>{" "}
                        is not a recognized runtime. Click Python or Deno above,
                        or edit{" "}
                        <code className="rounded bg-amber-500/20 px-1">
                          settings.json
                        </code>
                        .
                      </span>
                    </div>
                  )}
                </div>
              )}
            </div>

            {/* Python settings */}
            {(onDefaultPythonEnvChange ||
              onDefaultUvPackagesChange ||
              onDefaultCondaPackagesChange) && (
              <div className="space-y-2">
                <div>
                  <span className="text-xs font-semibold text-muted-foreground uppercase tracking-wider">
                    Python Defaults
                  </span>
                  <p className="text-[11px] text-muted-foreground/70 mt-0.5">
                    Applied to new notebooks without project-based dependencies
                  </p>
                </div>
                <div
                  className="grid gap-2"
                  style={{ gridTemplateColumns: "auto 1fr" }}
                >
                  {/* Default Python Env */}
                  {onDefaultPythonEnvChange && (
                    <>
                      <span className="text-xs font-medium text-muted-foreground whitespace-nowrap self-center text-right">
                        Environment
                      </span>
                      <div
                        className="flex items-center gap-1 rounded-md border bg-muted/50 p-0.5 w-fit"
                        data-testid="settings-python-env-group"
                      >
                        <button
                          type="button"
                          onClick={() => onDefaultPythonEnvChange("uv")}
                          className={cn(
                            "flex items-center gap-1.5 rounded-sm px-2.5 py-1 text-xs transition-colors",
                            defaultPythonEnv === "uv"
                              ? "bg-fuchsia-500/15 text-fuchsia-600 dark:text-fuchsia-400 shadow-sm"
                              : "text-muted-foreground hover:text-foreground",
                          )}
                        >
                          <UvIcon className="h-3 w-3" />
                          uv
                        </button>
                        <button
                          type="button"
                          onClick={() => onDefaultPythonEnvChange("conda")}
                          className={cn(
                            "flex items-center gap-1.5 rounded-sm px-2.5 py-1 text-xs transition-colors",
                            defaultPythonEnv === "conda"
                              ? "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400 shadow-sm"
                              : "text-muted-foreground hover:text-foreground",
                          )}
                        >
                          <CondaIcon className="h-3 w-3" />
                          Conda
                        </button>
                      </div>
                      {defaultPythonEnv &&
                        !isKnownPythonEnv(defaultPythonEnv) && (
                          <div className="flex items-start gap-2 text-xs text-amber-700 dark:text-amber-400 col-span-2 mt-1">
                            <AlertCircle className="h-3.5 w-3.5 mt-0.5 shrink-0" />
                            <span>
                              <span className="font-medium">
                                &ldquo;{defaultPythonEnv}&rdquo;
                              </span>{" "}
                              is not a recognized environment. Click uv or Conda
                              above, or edit{" "}
                              <code className="rounded bg-amber-500/20 px-1">
                                settings.json
                              </code>
                              .
                            </span>
                          </div>
                        )}
                    </>
                  )}

                  {/* Packages — show only the input matching the selected env */}
                  {defaultPythonEnv === "uv" && onDefaultUvPackagesChange && (
                    <>
                      <span className="text-xs font-medium text-muted-foreground whitespace-nowrap self-center text-right">
                        Packages
                      </span>
                      <PackageBadgeInput
                        packages={defaultUvPackages}
                        onChange={onDefaultUvPackagesChange}
                        placeholder="Add packages…"
                      />
                    </>
                  )}
                  {defaultPythonEnv === "conda" &&
                    onDefaultCondaPackagesChange && (
                      <>
                        <span className="text-xs font-medium text-muted-foreground whitespace-nowrap self-center text-right">
                          Packages
                        </span>
                        <PackageBadgeInput
                          packages={defaultCondaPackages}
                          onChange={onDefaultCondaPackagesChange}
                          placeholder="Add packages…"
                        />
                      </>
                    )}
                </div>
              </div>
            )}

            {/* Advanced settings */}
            {onKeepAliveSecsChange && (
              <KeepAliveSlider
                value={keepAliveSecs}
                onChange={onKeepAliveSecsChange}
              />
            )}
          </div>
        </CollapsibleContent>
      </header>
    </Collapsible>
  );
}
