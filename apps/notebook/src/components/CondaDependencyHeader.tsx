import {
  AlertCircle,
  Check,
  Download,
  FileText,
  Info,
  Plus,
  RefreshCw,
  X,
} from "lucide-react";
import { type KeyboardEvent, useCallback, useState } from "react";
import { Progress } from "@/components/ui/progress";
import type {
  CondaSyncState,
  EnvironmentYmlDeps,
  EnvironmentYmlInfo,
} from "../hooks/useCondaDependencies";
import type { EnvProgressState } from "../hooks/useEnvProgress";
import type { PixiInfo } from "../types";

interface CondaDependencyHeaderProps {
  dependencies: string[];
  channels: string[];
  python: string | null;
  loading: boolean;
  syncing: boolean;
  syncState: CondaSyncState | null;
  syncedWhileRunning: boolean;
  needsKernelRestart: boolean;
  onAdd: (pkg: string) => Promise<void>;
  onRemove: (pkg: string) => Promise<void>;
  onSetChannels: (channels: string[]) => Promise<void>;
  onSetPython: (python: string | null) => Promise<void>;
  onSyncNow: () => Promise<boolean>;
  /** Environment preparation progress state */
  envProgress?: EnvProgressState | null;
  /** Callback to reset/dismiss error state */
  onResetProgress?: () => void;
  // environment.yml support
  environmentYmlInfo?: EnvironmentYmlInfo | null;
  environmentYmlDeps?: EnvironmentYmlDeps | null;
  /** Detected pixi.toml info */
  pixiInfo?: PixiInfo | null;
  /** Import pixi.toml deps into notebook conda metadata */
  onImportFromPixi?: () => Promise<void>;
  /** Show success feedback after sync completed */
  justSynced?: boolean;
}

export function CondaDependencyHeader({
  dependencies,
  channels,
  python,
  loading,
  syncing,
  syncState,
  syncedWhileRunning,
  needsKernelRestart,
  onAdd,
  onRemove,
  onSetChannels,
  onSetPython,
  onSyncNow,
  envProgress,
  onResetProgress,
  environmentYmlInfo,
  environmentYmlDeps,
  pixiInfo,
  onImportFromPixi,
  justSynced,
}: CondaDependencyHeaderProps) {
  const [newDep, setNewDep] = useState("");
  const [newChannel, setNewChannel] = useState("");
  const [showChannelInput, setShowChannelInput] = useState(false);

  const handleAdd = useCallback(async () => {
    if (newDep.trim()) {
      await onAdd(newDep.trim());
      setNewDep("");
    }
  }, [newDep, onAdd]);

  const handleKeyDown = useCallback(
    (e: KeyboardEvent<HTMLInputElement>) => {
      if (e.key === "Enter") {
        e.preventDefault();
        handleAdd();
      }
    },
    [handleAdd],
  );

  const handleAddChannel = useCallback(async () => {
    if (newChannel.trim()) {
      const updated = [...channels, newChannel.trim()];
      await onSetChannels(updated);
      setNewChannel("");
      setShowChannelInput(false);
    }
  }, [newChannel, channels, onSetChannels]);

  const handleRemoveChannel = useCallback(
    async (channel: string) => {
      const updated = channels.filter((c) => c !== channel);
      await onSetChannels(updated);
    },
    [channels, onSetChannels],
  );

  const handlePythonChange = useCallback(
    async (e: React.ChangeEvent<HTMLInputElement>) => {
      const value = e.target.value.trim();
      await onSetPython(value || null);
    },
    [onSetPython],
  );

  // Default channels if none specified
  const displayChannels = channels.length > 0 ? channels : ["conda-forge"];

  // Calculate progress percentage
  const progressPercent =
    envProgress?.progress && envProgress.progress.total > 0
      ? (envProgress.progress.completed / envProgress.progress.total) * 100
      : 0;

  return (
    <div
      className="border-b bg-emerald-50/30 dark:bg-emerald-950/10"
      data-testid="conda-deps-panel"
    >
      <div className="px-3 py-3">
        {/* Conda badge */}
        <div className="mb-2 flex items-center gap-2">
          <span className="rounded bg-emerald-500/20 px-1.5 py-0.5 text-xs font-medium text-emerald-700 dark:text-emerald-400">
            conda
          </span>
        </div>

        {/* Environment preparation progress */}
        {envProgress?.isActive && (
          <div className="mb-3 rounded bg-muted/80 px-2 py-2">
            <div className="flex items-center justify-between mb-1">
              <span className="text-xs text-muted-foreground truncate">
                {envProgress.statusText}
              </span>
              {envProgress.progress && (
                <span className="text-xs text-emerald-600 dark:text-emerald-500 ml-2 shrink-0">
                  {envProgress.progress.completed}/{envProgress.progress.total}
                </span>
              )}
            </div>
            {envProgress.progress && (
              <Progress
                value={progressPercent}
                className="h-1.5 bg-emerald-200 dark:bg-emerald-900"
              />
            )}
          </div>
        )}

        {/* Error banner - persists until dismissed */}
        {envProgress?.error && (
          <div className="mb-3 rounded bg-red-500/10 border border-red-200 dark:border-red-900 px-2 py-2">
            <div className="flex items-start gap-2 text-xs text-red-700 dark:text-red-400">
              <AlertCircle className="h-4 w-4 mt-0.5 shrink-0" />
              <div className="flex-1 min-w-0">
                <div className="font-medium">Environment creation failed</div>
                <pre className="mt-1 whitespace-pre-wrap font-mono text-[10px] opacity-80 overflow-x-auto">
                  {envProgress.error}
                </pre>
              </div>
              {onResetProgress && (
                <button
                  type="button"
                  onClick={onResetProgress}
                  className="text-red-500 hover:text-red-700 dark:hover:text-red-300 shrink-0"
                  title="Dismiss"
                >
                  <X className="h-3 w-3" />
                </button>
              )}
            </div>
          </div>
        )}

        {/* Success feedback after sync completed */}
        {justSynced && (
          <div className="mb-3 flex items-center gap-2 rounded bg-emerald-500/10 px-2 py-1.5 text-xs text-emerald-700 dark:text-emerald-400">
            <Check className="h-3.5 w-3.5 shrink-0" />
            <span>Environment synced — dependencies are ready to use</span>
          </div>
        )}

        {/* Sync notice */}
        {syncedWhileRunning && (
          <div className="mb-3 flex items-start gap-2 rounded bg-muted/80 px-2 py-1.5 text-xs text-muted-foreground">
            <Info className="h-3.5 w-3.5 mt-0.5 shrink-0" />
            <span>
              Dependencies synced to environment. New packages can be imported
              now. Re-initialize the environment if you updated existing
              packages.
            </span>
          </div>
        )}

        {/* Kernel restart needed notice */}
        {needsKernelRestart && (
          <div className="mb-3 flex items-start gap-2 rounded bg-amber-500/10 px-2 py-1.5 text-xs text-amber-700 dark:text-amber-400">
            <Info className="h-3.5 w-3.5 mt-0.5 shrink-0" />
            <span>
              Re-initialize the environment to use these dependencies. Conda
              environments need a fresh environment after changes.
            </span>
          </div>
        )}

        {/* environment.yml detected banner */}
        {environmentYmlInfo?.has_dependencies && (
          <div className="mb-3 rounded bg-muted/80 px-2 py-1.5 text-xs text-muted-foreground">
            <div className="flex items-center gap-2">
              <FileText className="h-3.5 w-3.5 shrink-0" />
              <span>
                Using deps from{" "}
                <code className="rounded bg-muted px-1">
                  {environmentYmlInfo.relative_path}
                </code>
                {environmentYmlInfo.name && (
                  <span className="text-muted-foreground ml-1">
                    ({environmentYmlInfo.name})
                  </span>
                )}
              </span>
            </div>
            {environmentYmlDeps &&
              (environmentYmlDeps.dependencies.length > 0 ||
                environmentYmlDeps.pip_dependencies.length > 0) && (
                <div className="mt-2 text-xs text-muted-foreground">
                  {environmentYmlDeps.dependencies.length > 0 && (
                    <div className="flex flex-wrap gap-1 mb-1">
                      {environmentYmlDeps.dependencies.map((dep) => (
                        <span
                          key={dep}
                          className="rounded bg-muted px-1.5 py-0.5 font-mono"
                        >
                          {dep}
                        </span>
                      ))}
                    </div>
                  )}
                  {environmentYmlDeps.pip_dependencies.length > 0 && (
                    <div className="flex flex-wrap gap-1">
                      <span className="text-muted-foreground">pip:</span>
                      {environmentYmlDeps.pip_dependencies.map((dep) => (
                        <span
                          key={dep}
                          className="rounded bg-muted px-1.5 py-0.5 font-mono"
                        >
                          {dep}
                        </span>
                      ))}
                    </div>
                  )}
                </div>
              )}
          </div>
        )}

        {/* Environment drift notice - kernel restart needed */}
        {syncState?.status === "dirty" && !needsKernelRestart && (
          <div className="mb-3 flex items-center justify-between rounded bg-amber-500/10 px-2 py-1.5 text-xs text-amber-700 dark:text-amber-400">
            <div className="flex items-center gap-2">
              <Info className="h-3.5 w-3.5 shrink-0" />
              <span>
                Dependencies changed — re-initialize environment to apply
              </span>
            </div>
            <button
              type="button"
              onClick={onSyncNow}
              disabled={syncing}
              data-testid="deps-restart-button"
              className="flex items-center gap-1 rounded bg-amber-600 px-2 py-0.5 text-white text-xs font-medium hover:bg-amber-700 transition-colors disabled:opacity-50"
            >
              <RefreshCw
                className={`h-3 w-3 ${syncing ? "animate-spin" : ""}`}
              />
              Re-initialize
            </button>
          </div>
        )}

        {/* pixi.toml detected banner */}
        {pixiInfo?.has_dependencies && (
          <div className="mb-3 rounded bg-muted/80 px-2 py-1.5 text-xs text-muted-foreground">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <FileText className="h-3.5 w-3.5 shrink-0" />
                <span>
                  <code className="rounded bg-muted px-1">
                    {pixiInfo.relative_path}
                  </code>
                  {pixiInfo.workspace_name && (
                    <span className="text-muted-foreground ml-1">
                      ({pixiInfo.workspace_name})
                    </span>
                  )}
                  <span className="text-muted-foreground ml-1">
                    · {pixiInfo.dependency_count} dep
                    {pixiInfo.dependency_count !== 1 ? "s" : ""}
                  </span>
                </span>
              </div>
              {onImportFromPixi && (
                <button
                  type="button"
                  onClick={onImportFromPixi}
                  disabled={loading}
                  className="flex items-center gap-1 text-emerald-600/70 hover:text-emerald-700 dark:text-emerald-400/70 dark:hover:text-emerald-400 transition-colors disabled:opacity-50"
                  title="Copy pixi.toml deps into notebook metadata for portable sharing"
                >
                  <Download className="h-3 w-3" />
                  Copy to notebook
                </button>
              )}
            </div>
          </div>
        )}

        {/* Channels */}
        <div className="mb-2">
          <div className="mb-1 text-xs text-muted-foreground">Channels:</div>
          <div className="flex flex-wrap gap-1.5 items-center">
            {displayChannels.map((channel) => (
              <div
                key={channel}
                className="flex items-center gap-1 rounded bg-emerald-100 dark:bg-emerald-900/30 px-2 py-0.5 text-xs border border-emerald-200 dark:border-emerald-800"
              >
                <span className="font-mono">{channel}</span>
                {channels.length > 0 && (
                  <button
                    type="button"
                    onClick={() => handleRemoveChannel(channel)}
                    className="text-muted-foreground hover:text-foreground transition-colors"
                    disabled={loading}
                    title={`Remove ${channel}`}
                  >
                    <X className="h-3 w-3" />
                  </button>
                )}
              </div>
            ))}
            {showChannelInput ? (
              <div className="flex gap-1">
                <input
                  type="text"
                  value={newChannel}
                  onChange={(e) => setNewChannel(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      handleAddChannel();
                    } else if (e.key === "Escape") {
                      setShowChannelInput(false);
                      setNewChannel("");
                    }
                  }}
                  placeholder="channel name"
                  className="w-32 rounded border bg-background px-1.5 py-0.5 text-xs placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-primary"
                  autoFocus
                  disabled={loading}
                  autoComplete="off"
                  spellCheck={false}
                />
                <button
                  type="button"
                  onClick={handleAddChannel}
                  disabled={loading || !newChannel.trim()}
                  className="rounded bg-emerald-500 px-1.5 py-0.5 text-xs text-white hover:bg-emerald-600 disabled:opacity-50"
                >
                  Add
                </button>
              </div>
            ) : (
              <button
                type="button"
                onClick={() => setShowChannelInput(true)}
                className="flex items-center gap-0.5 rounded bg-background px-1.5 py-0.5 text-xs border hover:bg-muted transition-colors"
                disabled={loading}
              >
                <Plus className="h-3 w-3" />
                channel
              </button>
            )}
          </div>
        </div>

        {/* Python version */}
        <div className="mb-2 flex items-center gap-2">
          <span className="text-xs text-muted-foreground">Python:</span>
          <input
            type="text"
            value={python ?? ""}
            onChange={handlePythonChange}
            placeholder="3.11"
            className="w-20 rounded border bg-background px-1.5 py-0.5 text-xs font-mono placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-primary"
            disabled={loading}
            autoComplete="off"
            spellCheck={false}
          />
        </div>

        {/* Dependencies list */}
        {dependencies.length > 0 ? (
          <div className="mb-3 flex flex-wrap gap-1.5">
            {dependencies.map((dep) => (
              <div
                key={dep}
                className="flex items-center gap-1 rounded bg-background px-2 py-1 text-xs border"
              >
                <span className="font-mono">{dep}</span>
                <button
                  type="button"
                  onClick={() => onRemove(dep)}
                  className="text-muted-foreground hover:text-foreground transition-colors"
                  disabled={loading}
                  title={`Remove ${dep}`}
                >
                  <X className="h-3 w-3" />
                </button>
              </div>
            ))}
          </div>
        ) : (
          <div className="mb-3 text-xs text-muted-foreground">
            No dependencies. Add conda packages to create an isolated
            environment.
          </div>
        )}

        {/* Add dependency input */}
        <div className="flex gap-2">
          <input
            type="text"
            value={newDep}
            onChange={(e) => setNewDep(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder="package or package>=version"
            data-testid="conda-deps-add-input"
            className="flex-1 rounded border bg-background px-2 py-1 text-xs placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-primary"
            disabled={loading}
            autoComplete="off"
            spellCheck={false}
          />
          <button
            type="button"
            onClick={handleAdd}
            disabled={loading || !newDep.trim()}
            data-testid="conda-deps-add-button"
            className="flex items-center gap-1 rounded bg-emerald-600 px-2 py-1 text-xs text-white transition-colors hover:bg-emerald-700 disabled:opacity-50"
          >
            <Plus className="h-3 w-3" />
            Add
          </button>
        </div>
      </div>
    </div>
  );
}
