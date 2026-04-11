import { Check, FileText, Info, Package, Plus, RefreshCw, Terminal, X } from "lucide-react";
import { type KeyboardEvent, useCallback, useState } from "react";
import type { EnvSyncState } from "../hooks/useDependencies";
import { addPixiDependency, removePixiDependency, usePixiDeps } from "../lib/notebook-metadata";
import type { PixiInfo } from "../types";
import { PixiIcon } from "./icons";

interface PixiDependencyHeaderProps {
  pixiInfo: PixiInfo | null;
  envSource: string | null;
  syncState?: EnvSyncState | null;
  onSyncNow?: () => Promise<boolean>;
  justSynced?: boolean;
}

export function PixiDependencyHeader({
  pixiInfo,
  envSource,
  syncState,
  onSyncNow,
  justSynced,
}: PixiDependencyHeaderProps) {
  const pixiDeps = usePixiDeps();
  const isInlineMode = envSource === "pixi:inline" || envSource === "pixi:prewarmed";
  const [newDep, setNewDep] = useState("");
  const [loading, setLoading] = useState(false);

  const handleAdd = useCallback(async () => {
    if (newDep.trim()) {
      setLoading(true);
      try {
        await addPixiDependency(newDep.trim());
      } finally {
        setLoading(false);
      }
      setNewDep("");
    }
  }, [newDep]);

  const handleKeyDown = useCallback(
    (e: KeyboardEvent) => {
      if (e.key === "Enter") {
        e.preventDefault();
        handleAdd();
      }
    },
    [handleAdd],
  );

  const handleRemove = useCallback(async (pkg: string) => {
    setLoading(true);
    try {
      await removePixiDependency(pkg);
    } finally {
      setLoading(false);
    }
  }, []);

  return (
    <div className="border-b bg-amber-50/30 dark:bg-amber-950/10">
      <div className="px-3 py-3">
        {/* Pixi badge */}
        <div className="mb-2 flex items-center gap-2">
          <span className="flex items-center gap-1 rounded bg-amber-500/20 px-1.5 py-0.5 text-xs font-medium text-amber-600 dark:text-amber-400">
            <PixiIcon className="h-2.5 w-2.5" />
            Pixi
          </span>
          <span className="text-xs text-muted-foreground">
            {isInlineMode ? "Dependencies" : "Environment"}
          </span>
        </div>

        {/* Success feedback after sync */}
        {justSynced && (
          <div className="mb-3 flex items-center gap-2 rounded bg-amber-500/10 px-2 py-1.5 text-xs text-amber-700 dark:text-amber-400">
            <Check className="h-3.5 w-3.5 shrink-0" />
            <span>Kernel restarted — environment updated</span>
          </div>
        )}

        {/* Sync state drift banner */}
        {syncState?.status === "dirty" && onSyncNow && (
          <div className="mb-3 flex items-center justify-between rounded bg-amber-500/10 px-2 py-1.5 text-xs text-amber-700 dark:text-amber-400">
            <div className="flex items-center gap-2">
              <Info className="h-3.5 w-3.5 shrink-0" />
              <span>
                Dependencies changed
                {syncState.added.length > 0 && ` — ${syncState.added.length} added`}
                {syncState.removed.length > 0 && ` — ${syncState.removed.length} removed`}
              </span>
            </div>
            <button
              type="button"
              onClick={onSyncNow}
              disabled={loading}
              className="flex items-center gap-1 rounded bg-amber-600 px-2 py-0.5 text-white text-xs font-medium hover:bg-amber-700 transition-colors disabled:opacity-50"
            >
              <RefreshCw className={`h-3 w-3 ${loading ? "animate-spin" : ""}`} />
              Restart
            </button>
          </div>
        )}

        {/* pixi.toml detected banner (pixi:toml mode) */}
        {pixiInfo && !isInlineMode && (
          <div className="mb-3 rounded bg-muted/80 px-2 py-1.5 text-xs text-muted-foreground">
            <div className="flex items-center gap-2">
              <FileText className="h-3.5 w-3.5 shrink-0" />
              <span>
                Using <code className="rounded bg-muted px-1">{pixiInfo.relative_path}</code>
                {pixiInfo.workspace_name && (
                  <span className="text-muted-foreground ml-1">({pixiInfo.workspace_name})</span>
                )}
              </span>
            </div>

            {/* Show dep names from CRDT (bootstrapped from pixi.toml) */}
            {pixiDeps?.dependencies?.length ? (
              <div className="mt-1.5 flex flex-wrap gap-1.5">
                {pixiDeps.dependencies.map((dep) => (
                  <span
                    key={dep}
                    className="rounded bg-muted px-1.5 py-0.5 font-mono text-muted-foreground"
                  >
                    {dep}
                  </span>
                ))}
              </div>
            ) : null}
            {pixiDeps?.pypiDependencies?.length ? (
              <div className="mt-1.5 flex flex-wrap gap-1.5">
                <span className="text-muted-foreground text-[10px] uppercase tracking-wide self-center">
                  PyPI
                </span>
                {pixiDeps.pypiDependencies.map((dep) => (
                  <span
                    key={dep}
                    className="rounded bg-muted px-1.5 py-0.5 font-mono text-muted-foreground"
                  >
                    {dep}
                  </span>
                ))}
              </div>
            ) : null}
            {/* Fallback: show counts if CRDT deps not yet bootstrapped */}
            {(!pixiDeps || pixiDeps.dependencies.length === 0) &&
              (pixiInfo.has_dependencies || pixiInfo.has_pypi_dependencies) && (
                <div className="mt-1.5 flex gap-2 text-muted-foreground">
                  {pixiInfo.has_dependencies && (
                    <span className="rounded bg-muted px-1.5 py-0.5">
                      {pixiInfo.dependency_count} conda dep
                      {pixiInfo.dependency_count !== 1 ? "s" : ""}
                    </span>
                  )}
                  {pixiInfo.has_pypi_dependencies && (
                    <span className="rounded bg-muted px-1.5 py-0.5">
                      {pixiInfo.pypi_dependency_count} pypi dep
                      {pixiInfo.pypi_dependency_count !== 1 ? "s" : ""}
                    </span>
                  )}
                </div>
              )}

            {pixiInfo.channels.length > 0 && (
              <div className="mt-1.5 flex items-center gap-1.5 text-muted-foreground">
                <Package className="h-3 w-3 shrink-0" />
                {pixiInfo.channels.map((ch) => (
                  <span key={ch} className="rounded bg-muted px-1.5 py-0.5">
                    {ch}
                  </span>
                ))}
              </div>
            )}

            {pixiInfo.python && (
              <div className="mt-1.5 text-muted-foreground">Python: {pixiInfo.python}</div>
            )}
          </div>
        )}

        {/* Inline deps list + add/remove (pixi:inline / pixi:prewarmed mode) */}
        {isInlineMode && (
          <>
            {/* Current inline deps */}
            {pixiDeps && pixiDeps.dependencies.length > 0 && (
              <div className="mb-2 flex flex-wrap gap-1.5">
                {pixiDeps.dependencies.map((dep) => (
                  <div
                    key={dep}
                    className="flex items-center gap-1 rounded bg-amber-100 dark:bg-amber-900/30 px-2 py-0.5 text-xs border border-amber-200 dark:border-amber-800"
                  >
                    <span>{dep}</span>
                    <button
                      type="button"
                      onClick={() => handleRemove(dep)}
                      disabled={loading}
                      className="text-amber-500/50 hover:text-amber-700 dark:hover:text-amber-300 transition-colors disabled:opacity-50"
                    >
                      <X className="h-3 w-3" />
                    </button>
                  </div>
                ))}
              </div>
            )}

            {/* Add dep input */}
            <div className="mb-3 flex gap-1.5">
              <input
                type="text"
                value={newDep}
                onChange={(e) => setNewDep(e.target.value)}
                onKeyDown={handleKeyDown}
                placeholder="Add conda package..."
                className="flex-1 rounded border bg-background px-2 py-1 text-xs focus:outline-none focus:ring-1 focus:ring-amber-500"
              />
              <button
                type="button"
                onClick={handleAdd}
                disabled={loading || !newDep.trim()}
                className="flex items-center gap-1 rounded bg-amber-500/20 px-2 py-1 text-xs text-amber-600 dark:text-amber-400 hover:bg-amber-500/30 transition-colors disabled:opacity-50"
              >
                <Plus className="h-3 w-3" />
                Add
              </button>
            </div>
          </>
        )}

        {/* No pixi.toml and no inline deps — show init tip */}
        {!pixiInfo && (!pixiDeps || pixiDeps.dependencies.length === 0) && (
          <div className="mb-3 flex items-start gap-2 rounded bg-muted/50 px-2 py-1.5 text-xs text-muted-foreground">
            <Info className="h-3.5 w-3.5 mt-0.5 shrink-0" />
            <span>
              No <code className="rounded bg-muted px-1">pixi.toml</code> found. Add packages above
              or run <code className="rounded bg-muted px-1">pixi init</code> in your terminal.
            </span>
          </div>
        )}

        {/* Tip for pixi:toml mode */}
        {!isInlineMode && (
          <div className="flex items-start gap-2 rounded bg-muted/50 px-2 py-1.5 text-xs text-muted-foreground">
            <Terminal className="h-3.5 w-3.5 mt-0.5 shrink-0" />
            <span>
              Manage dependencies with{" "}
              <code className="rounded bg-muted px-1">pixi add &lt;package&gt;</code> in your
              terminal.
            </span>
          </div>
        )}
      </div>
    </div>
  );
}
