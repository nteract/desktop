import {
  Check,
  Download,
  FileText,
  Info,
  Plus,
  RefreshCw,
  X,
} from "lucide-react";
import { type KeyboardEvent, useCallback, useState } from "react";
import type {
  EnvSyncState,
  PyProjectDeps,
  PyProjectInfo,
} from "../hooks/useDependencies";

interface DependencyHeaderProps {
  dependencies: string[];
  requiresPython: string | null;
  loading: boolean;
  syncedWhileRunning: boolean;
  needsKernelRestart: boolean;
  onAdd: (pkg: string) => Promise<void>;
  onRemove: (pkg: string) => Promise<void>;
  // Environment sync state
  syncState?: EnvSyncState | null;
  onSyncNow?: () => Promise<boolean>;
  // pyproject.toml support
  pyprojectInfo?: PyProjectInfo | null;
  pyprojectDeps?: PyProjectDeps | null;
  /** Copy pyproject.toml deps into notebook metadata as a portable snapshot */
  onImportFromPyproject?: () => Promise<void>;
  /** Start kernel using the project environment (uv run) */
  onUseProjectEnv?: () => Promise<void>;
  /** Whether the kernel is currently using the project environment */
  isUsingProjectEnv?: boolean;
  /** Show success feedback after sync completed */
  justSynced?: boolean;
}

export function DependencyHeader({
  dependencies,
  requiresPython,
  loading,
  syncedWhileRunning,
  needsKernelRestart,
  onAdd,
  onRemove,
  syncState,
  onSyncNow,
  pyprojectInfo,
  pyprojectDeps,
  onImportFromPyproject,
  onUseProjectEnv,
  isUsingProjectEnv,
  justSynced,
}: DependencyHeaderProps) {
  const [newDep, setNewDep] = useState("");

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

  return (
    <div
      className="border-b bg-uv/[0.02] dark:bg-uv/[0.04]"
      data-testid="deps-panel"
    >
      <div className="px-3 py-3">
        {/* uv badge */}
        <div className="mb-2 flex items-center gap-2">
          <span className="rounded bg-uv/20 px-1.5 py-0.5 text-xs font-medium text-uv">
            uv
          </span>
        </div>

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
              Re-initialize the environment to use these dependencies. The
              current kernel wasn&apos;t started with dependency management.
            </span>
          </div>
        )}

        {/* Environment drift notice - kernel restart needed */}
        {syncState?.status === "dirty" && onSyncNow && (
          <div className="mb-3 flex items-center justify-between rounded bg-amber-500/10 px-2 py-1.5 text-xs text-amber-700 dark:text-amber-400">
            <div className="flex items-center gap-2">
              <Info className="h-3.5 w-3.5 shrink-0" />
              <span>
                Re-initialize the environment to use{" "}
                {syncState.added.length > 0 && (
                  <span>
                    {syncState.added.length} new package
                    {syncState.added.length > 1 ? "s" : ""}
                  </span>
                )}
                {syncState.added.length > 0 &&
                  syncState.removed.length > 0 &&
                  " and remove "}
                {syncState.removed.length > 0 && (
                  <span>
                    {syncState.removed.length} package
                    {syncState.removed.length > 1 ? "s" : ""}
                  </span>
                )}
              </span>
            </div>
            <button
              type="button"
              onClick={onSyncNow}
              disabled={loading}
              data-testid="deps-restart-button"
              className="flex items-center gap-1 rounded bg-amber-600 px-2 py-0.5 text-white text-xs font-medium hover:bg-amber-700 transition-colors disabled:opacity-50"
            >
              <RefreshCw
                className={`h-3 w-3 ${loading ? "animate-spin" : ""}`}
              />
              Re-initialize
            </button>
          </div>
        )}

        {/* pyproject.toml detected banner */}
        {pyprojectInfo?.has_dependencies && (
          <div className="mb-3 rounded bg-muted/80 px-2 py-1.5 text-xs text-muted-foreground">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <FileText className="h-3.5 w-3.5 shrink-0" />
                <span>
                  <code className="rounded bg-muted px-1">
                    {pyprojectInfo.relative_path}
                  </code>
                  {pyprojectInfo.project_name && (
                    <span className="text-muted-foreground ml-1">
                      ({pyprojectInfo.project_name})
                    </span>
                  )}
                </span>
              </div>
              <div className="flex items-center gap-2">
                {onUseProjectEnv && !isUsingProjectEnv && (
                  <button
                    type="button"
                    onClick={onUseProjectEnv}
                    disabled={loading}
                    className="flex items-center gap-1 rounded bg-uv px-2 py-0.5 text-white text-xs font-medium hover:bg-uv/90 transition-colors disabled:opacity-50"
                    title="Start kernel with uv run — stays in sync with pyproject.toml"
                  >
                    Use project env
                  </button>
                )}
                {isUsingProjectEnv && (
                  <span className="rounded bg-uv/20 px-1.5 py-0.5 text-uv text-xs font-medium">
                    Active
                  </span>
                )}
                {onImportFromPyproject && (
                  <button
                    type="button"
                    onClick={onImportFromPyproject}
                    disabled={loading}
                    className="flex items-center gap-1 text-uv/70 hover:text-uv transition-colors disabled:opacity-50"
                    title="Copy deps into notebook metadata for portable sharing"
                  >
                    <Download className="h-3 w-3" />
                    Copy to notebook
                  </button>
                )}
              </div>
            </div>
            {pyprojectDeps &&
              (pyprojectDeps.dependencies.length > 0 ||
                pyprojectDeps.dev_dependencies.length > 0) && (
                <div className="mt-2 text-xs text-muted-foreground">
                  {pyprojectDeps.dependencies.length > 0 && (
                    <div className="flex flex-wrap gap-1 mb-1">
                      {pyprojectDeps.dependencies.map((dep) => (
                        <span
                          key={dep}
                          className="rounded bg-muted px-1.5 py-0.5 font-mono"
                        >
                          {dep}
                        </span>
                      ))}
                    </div>
                  )}
                  {pyprojectDeps.dev_dependencies.length > 0 && (
                    <div className="flex flex-wrap gap-1">
                      <span className="text-muted-foreground">dev:</span>
                      {pyprojectDeps.dev_dependencies.map((dep) => (
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

        {/* Project-managed state: read-only view when using uv run */}
        {isUsingProjectEnv && (
          <div className="mb-3 flex items-start gap-2 rounded bg-muted/80 px-2 py-1.5 text-xs text-muted-foreground">
            <Info className="h-3.5 w-3.5 mt-0.5 shrink-0" />
            <span>
              Managed by{" "}
              <code className="rounded bg-muted px-1">
                {pyprojectInfo?.relative_path ?? "pyproject.toml"}
              </code>{" "}
              — re-initialize the environment to pick up dependency changes.
            </span>
          </div>
        )}

        {/* Python version */}
        {requiresPython && (
          <div className="mb-2 text-xs text-muted-foreground">
            Python: <span className="font-mono">{requiresPython}</span>
          </div>
        )}

        {/* Dependencies list (read-only when using project env) */}
        {!isUsingProjectEnv &&
          (dependencies.length > 0 ? (
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
              No inline dependencies. Add packages to create an isolated
              environment.
            </div>
          ))}

        {/* Add dependency input (hidden when using project env) */}
        {!isUsingProjectEnv && (
          <div className="flex gap-2">
            <input
              type="text"
              value={newDep}
              onChange={(e) => setNewDep(e.target.value)}
              onKeyDown={handleKeyDown}
              placeholder="package or package>=version"
              data-testid="deps-add-input"
              className="flex-1 rounded border bg-background px-2 py-1 text-xs placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-primary"
              disabled={loading}
              autoComplete="off"
              spellCheck={false}
            />
            <button
              type="button"
              onClick={handleAdd}
              disabled={loading || !newDep.trim()}
              data-testid="deps-add-button"
              className="flex items-center gap-1 rounded bg-uv px-2 py-1 text-xs text-white transition-colors hover:bg-uv/90 disabled:opacity-50"
            >
              <Plus className="h-3 w-3" />
              Add
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
