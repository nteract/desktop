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
import type { ThemeMode } from "@/hooks/useSyncedSettings";
import { isKnownPythonEnv, isKnownRuntime } from "@/hooks/useSyncedSettings";
import { cn } from "@/lib/utils";
import type { EnvProgressState } from "../hooks/useEnvProgress";
import type { UpdateStatus } from "../hooks/useUpdater";
import type { KernelspecInfo } from "../types";

/** Deno logo icon (from tabler icons) */
function DenoIcon({ className }: { className?: string }) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
    >
      <path d="M3 12a9 9 0 1 0 18 0a9 9 0 1 0 -18 0" />
      <path d="M13.47 20.882l-1.47 -5.882c-2.649 -.088 -5 -1.624 -5 -3.5c0 -1.933 2.239 -3.5 5 -3.5s4 1 5 3c.024 .048 .69 2.215 2 6.5" />
      <path d="M12 11h.01" />
    </svg>
  );
}

/** Python logo icon (from tabler icons) */
function PythonIcon({ className }: { className?: string }) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
    >
      <path d="M12 9h-7a2 2 0 0 0 -2 2v4a2 2 0 0 0 2 2h3" />
      <path d="M12 15h7a2 2 0 0 0 2 -2v-4a2 2 0 0 0 -2 -2h-3" />
      <path d="M8 9v-4a2 2 0 0 1 2 -2h4a2 2 0 0 1 2 2v5a2 2 0 0 1 -2 2h-4a2 2 0 0 0 -2 2v5a2 2 0 0 0 2 2h4a2 2 0 0 0 2 -2v-4" />
      <path d="M11 6l0 .01" />
      <path d="M13 18l0 .01" />
    </svg>
  );
}

/** uv logo icon */
function UvIcon({ className }: { className?: string }) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 41 41"
      fill="currentColor"
      className={className}
    >
      <path d="M-5.28619e-06 0.168629L0.0843098 20.1685L0.151762 36.1683C0.161075 38.3774 1.95947 40.1607 4.16859 40.1514L20.1684 40.084L30.1684 40.0418L31.1852 40.0375C33.3877 40.0282 35.1683 38.2026 35.1683 36V36L37.0003 36L37.0003 39.9992L40.1683 39.9996L39.9996 -9.94653e-07L21.5998 0.0775689L21.6774 16.0185L21.6774 25.9998L20.0774 25.9998L18.3998 25.9998L18.4774 16.032L18.3998 0.0910593L-5.28619e-06 0.168629Z" />
    </svg>
  );
}

/** Conda logo icon (from conda/conda repo) */
function CondaIcon({ className }: { className?: string }) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 23.565 27.149"
      fill="currentColor"
      stroke="currentColor"
      strokeWidth="0.25px"
      className={className}
    >
      <path
        d="M47.3 23.46c-.69-1.27-2.08-3.82-4.51-6.25-.81 3.47-.81 6.48-.69 7.99-.12 0 2.2-1.16 5.2-1.74zM36.54 28.32c-1.27-1.04-3.7-2.89-7.28-4.4.92 3.82 2.43 6.6 3.24 7.99 0 .23 1.62-1.74 4.05-3.59zM50.67 20.08c.81-2.08 2.31-5.09 4.74-8.22-1.5-2.66-3.58-5.32-6.36-7.64-2.2 2.78-3.7 5.56-4.74 8.33 3.12 2.66 5.09 5.44 6.36 7.52zM29.15 36.66c-1.22-.22-2.56-.41-4-.52a40.19 40.19 0 0 0-5.77-.06c1.01 1.29 2.24 2.71 3.73 4.14A39.43 39.43 0 0 0 26.35 43c.35-1.03.77-2.12 1.28-3.28a43.76 43.76 0 0 1 1.51-3.06zM11.92 49.15c4.16-2.66 8.09-3.82 10.75-4.17-2.08-1.74-5.09-4.51-7.52-8.33-3.47.69-7.01 2.02-10.36 4.33 1.97 3.47 4.47 6.2 7.12 8.17zM25.21 48.11c-1.62.12-5.56.34-10.19 3.12 4.28 2.66 8.22 4.06 10.19 4.52-.35-2.55-.35-5.21 0-7.64zM39.21 14.02c-2.54-1.74-5.78-3.24-9.83-4.17-.81 3.47-.92 6.71-.69 9.49 3.93 1.27 6.94 3.12 9.02 4.63 0-2.31.35-5.9 1.5-9.95z"
        fillRule="evenodd"
        transform="matrix(.26458 0 0 .26458 -.189 -.253)"
      />
      <path
        d="M14.22 17.73c1.52-.19 3.32-.3 5.35-.22 1.82.08 3.44.3 4.83.56.29-3.12.58-6.24.88-9.37a45.326 45.326 0 0 0-5.82 4.02 45.63 45.63 0 0 0-5.23 5z"
        fillRule="evenodd"
        strokeLinecap="round"
        transform="matrix(.26458 0 0 .26458 -.189 -.253)"
      />
      <path
        d="M31.2 5.66c2.19.64 4.61 1.55 7.12 2.84.75.38 1.46.78 2.13 1.17a33.466 33.466 0 0 1 4.63-8.01c-2.15.36-4.53.88-7.08 1.62-2.53.73-4.8 1.55-6.8 2.38Z"
        transform="matrix(.26458 0 0 .26458 -.189 -.253)"
      />
      <path
        d="M9.14 51.23c-2.66-2.2-5.32-5.09-7.4-8.68C.58 48.45.58 54.7 1.51 60.49c2.2-4.05 4.86-6.94 7.63-9.26ZM86.84 81.09c-7.28-6.94-8.79-11.46-14.91-6.71C55.17 87.57 31 79.7 25.91 59.22c-.92-.12-7.4-1.39-13.99-5.9-3.24 2.55-6.59 6.48-9.37 12.15h-.12c10.52 39.81 61.74 49.07 85.44 24.19 3.93-4.17.35-7.06-1.04-8.56zM69.26 6.58c-3.35 1.62-6.01 3.7-8.09 5.9 1.73 3.7 2.54 7.06 3.01 9.14 3.24-2.55 6.47-4.05 8.44-4.86-.23-2.31-1.04-6.6-3.35-10.18zM57.59 16.75a26.405 26.405 0 0 0-3.02 6.27c.8.04 1.66.12 2.56.23.82.1 1.6.23 2.32.38-.13-.88-.3-1.84-.55-2.86-.38-1.52-.84-2.87-1.32-4.02zM58.67 8.08c1.04-.98 2.3-2.05 3.78-3.1 1.32-.94 2.58-1.7 3.74-2.31-1.92-.46-4.1-.88-6.51-1.17-2.52-.3-4.83-.4-6.88-.38 2.42 2.21 4.38 4.64 5.87 6.96z"
        fillRule="evenodd"
        transform="matrix(.26458 0 0 .26458 -.189 -.253)"
      />
      <path
        d="M74.58 5.55s3.24 9.95 3.35 15.04c-3.93.93-7.4 2.31-11.21 5.56 2.08 1.04 3.93 2.31 5.67 3.82 1.85 1.62 3.58 2.31 6.01 1.16 1.39-.93 9.13-8.91 9.94-9.95 1.85-1.97 1.39-5.21-.58-6.71-4.86-4.51-13.18-8.91-13.18-8.91ZM2.82 37.77c3.47-2.31 6.94-3.82 10.41-4.63-1.5-3.12-2.54-6.71-2.77-10.88C7.11 27 4.45 32.33 2.83 37.77zM28.59 32.72c-1.16-2.2-2.66-5.79-3.47-10.18-3.12-.69-6.59-1.04-10.64-.46.23 4.05 1.39 7.52 2.89 10.53 4.62-.69 8.56-.35 11.22.12z"
        fillRule="evenodd"
        transform="matrix(.26458 0 0 .26458 -.189 -.253)"
      />
    </svg>
  );
}

/** Prefix/Pixi logo icon (P mark from prefix.dev) */
function PixiIcon({ className }: { className?: string }) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 27.4 40.2"
      fill="currentColor"
      className={className}
    >
      <path d="M27.116 3.67449C27.0374 1.63273 25.7268 0.270764 23.633 0.19141C16.9597-0.0609578 10.2854-0.0667476 3.61182 0.19175C1.56495 0.270764 0.211498 1.63273 0.128738 3.67449C0.0507459 5.60828 0.00647096 8.27738 0 9.98946C0 10.843 0.666168 11.488 1.56086 11.488C2.04414 11.488 2.46169 11.2956 2.74845 10.988C2.7539 10.985 2.76174 10.9846 2.76548 10.9802C3.03897 10.6791 3.2348 10.3079 3.50385 10.001C3.83217 9.62675 4.21021 9.28515 4.63219 9.0195C5.45945 8.49842 6.37151 8.20655 7.44535 8.20655C10.2694 8.20655 12.5673 10.2745 12.5673 13.3496C12.5673 16.4638 10.3297 18.5679 7.27779 18.5679C5.44514 18.5679 3.82195 17.7733 2.92351 16.2333C2.91193 16.2132 2.89695 16.1978 2.88469 16.1791C2.8789 16.1709 2.87311 16.1631 2.86732 16.1549C2.83019 16.1028 2.78932 16.0558 2.74505 16.0139C2.45862 15.7088 2.0421 15.518 1.5612 15.518C0.777537 15.518 0.169607 16.0132 0.0306519 16.7094C0.00919558 16.7714 0 16.8555 0.000340577 17.0162C0.00613038 18.7246 0.0527894 21.6614 0.129079 23.6953C0.166542 24.6952 0.572169 25.6696 1.34017 26.3269C2.21647 27.0772 3.21028 27.0776 4.28207 27.272C4.6516 27.3391 5.02215 27.4587 5.30585 27.7148C5.7159 28.0853 5.86712 28.6974 5.73531 29.2338C5.59125 29.8185 5.17268 30.2007 4.69996 30.5327C4.26232 30.8403 3.85874 31.1689 3.5168 31.5837C2.78694 32.4686 2.4089 33.5853 2.4089 34.7293C2.4089 37.8435 4.56986 40.0764 7.72326 40.0764C10.8375 40.0764 13.0836 37.9808 13.0836 35.0426C13.0836 33.8976 12.7699 32.7509 12.0912 31.8191C11.7683 31.376 11.3708 30.9935 10.9213 30.6809C10.47 30.3672 10.0136 30.1349 9.78409 29.5989C9.65433 29.2954 9.6104 28.9531 9.68362 28.6296C10.0068 27.1981 11.6052 27.3551 12.7233 27.3592C14.2909 27.365 15.8586 27.3579 17.426 27.3364C19.4956 27.3081 21.565 27.2557 23.6333 27.178C25.5899 27.1045 27.0377 25.6999 27.1164 23.695C27.3783 17.0217 27.3735 10.3474 27.1164 3.67381Z" />
    </svg>
  );
}

/** Badge color variant for environment sources */
type EnvBadgeVariant = "uv" | "conda" | "pixi";

interface NotebookToolbarProps {
  kernelStatus: string;
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
  onSave: () => void;
  onStartKernel: (name: string) => void;
  onInterruptKernel: () => void;
  onRestartKernel: () => void;
  onRunAllCells: () => void;
  onRestartAndRunAll: () => void;
  onAddCell: (type: "code" | "markdown") => void;
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

  const addPackages = useCallback(
    (raw: string) => {
      const names = raw
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean);
      if (names.length === 0) return;
      const unique = names.filter((n) => !packages.includes(n));
      if (unique.length > 0) {
        onChange([...packages, ...unique]);
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
          if (e.key === "Enter" || e.key === ",") {
            e.preventDefault();
            addPackages(inputValue);
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
            addPackages(inputValue);
          }
        }}
        onPaste={(e) => {
          const text = e.clipboardData.getData("text");
          if (text.includes(",")) {
            e.preventDefault();
            addPackages(text);
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
  onSave,
  onStartKernel,
  onInterruptKernel,
  onRestartKernel,
  onRunAllCells,
  onRestartAndRunAll,
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
    kernelStatus === "idle" ||
    kernelStatus === "busy" ||
    kernelStatus === "starting";

  // Derive env manager label for the runtime pill (e.g. "uv", "conda", "pixi")
  const envManager: EnvBadgeVariant | null =
    runtime === "python"
      ? envSource && (kernelStatus === "idle" || kernelStatus === "busy")
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
        className="sticky top-0 z-10 border-b bg-background/95 backdrop-blur supports-backdrop-filter:bg-background/60"
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
            onClick={() => onAddCell("code")}
            className="flex items-center gap-1 rounded px-2 py-1 text-xs text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
            title="Add code cell"
            data-testid="add-code-cell-button"
          >
            <Plus className="h-3 w-3" />
            Code
          </button>
          <button
            type="button"
            onClick={() => onAddCell("markdown")}
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
                kernelStatus === "busy"
                  ? "text-destructive hover:bg-destructive/10"
                  : "text-foreground hover:bg-muted",
              )}
              title="Interrupt kernel"
              data-testid="interrupt-kernel-button"
            >
              <Square
                className="h-3 w-3"
                fill={kernelStatus === "busy" ? "currentColor" : "none"}
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
            className="flex w-[3rem] items-center gap-1.5"
            role="status"
            aria-label={`Kernel: ${
              envProgress?.isActive
                ? envProgress.statusText
                : envProgress?.error
                  ? envProgress.statusText
                  : kernelStatus === "error" && kernelErrorMessage
                    ? `Error \u2014 ${kernelErrorMessage}`
                    : kernelStatus
            }`}
            title={
              envProgress?.isActive
                ? envProgress.statusText
                : envProgress?.error
                  ? envProgress.error
                  : kernelStatus === "error" && kernelErrorMessage
                    ? `Error \u2014 ${kernelErrorMessage}`
                    : kernelStatus
            }
          >
            <div
              className={cn(
                "h-2 w-2 shrink-0 rounded-full",
                kernelStatus === "idle" && "bg-green-500",
                kernelStatus === "busy" && "bg-amber-500",
                kernelStatus === "starting" && "bg-blue-500 animate-pulse",
                kernelStatus === "not started" &&
                  "bg-gray-400 dark:bg-gray-500",
                kernelStatus === "error" && "bg-red-500",
              )}
            />
            <span className="text-xs text-muted-foreground truncate">
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
                    kernelStatus === "error" &&
                      "text-red-600 dark:text-red-400",
                  )}
                >
                  {kernelStatus === "not started"
                    ? "off"
                    : kernelStatus === "starting"
                      ? "init"
                      : kernelStatus}
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
          kernelStatus === "error" &&
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
          </div>
        </CollapsibleContent>
      </header>
    </Collapsible>
  );
}
