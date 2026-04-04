import { FileText, Info, Package, Terminal } from "lucide-react";
import type { PixiInfo } from "../types";
import { PixiIcon } from "./icons";

interface PixiDependencyHeaderProps {
  pixiInfo: PixiInfo | null;
}

export function PixiDependencyHeader({ pixiInfo }: PixiDependencyHeaderProps) {
  return (
    <div className="border-b bg-amber-50/30 dark:bg-amber-950/10">
      <div className="px-3 py-3">
        {/* Pixi badge */}
        <div className="mb-2 flex items-center gap-2">
          <span className="flex items-center gap-1 rounded bg-amber-500/20 px-1.5 py-0.5 text-xs font-medium text-amber-600 dark:text-amber-400">
            <PixiIcon className="h-2.5 w-2.5" />
            Pixi
          </span>
          <span className="text-xs text-muted-foreground">Environment</span>
        </div>

        {/* pixi.toml detected banner */}
        {pixiInfo && (
          <div className="mb-3 rounded bg-muted/80 px-2 py-1.5 text-xs text-muted-foreground">
            <div className="flex items-center gap-2">
              <FileText className="h-3.5 w-3.5 shrink-0" />
              <span>
                Using{" "}
                <code className="rounded bg-muted px-1">
                  {pixiInfo.relative_path}
                </code>
                {pixiInfo.workspace_name && (
                  <span className="text-muted-foreground ml-1">
                    ({pixiInfo.workspace_name})
                  </span>
                )}
              </span>
            </div>

            {/* Dependency summary */}
            {(pixiInfo.has_dependencies || pixiInfo.has_pypi_dependencies) && (
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

            {/* Channels */}
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

            {/* Python version */}
            {pixiInfo.python && (
              <div className="mt-1.5 text-muted-foreground">
                Python: {pixiInfo.python}
              </div>
            )}
          </div>
        )}

        {/* No pixi.toml found */}
        {!pixiInfo && (
          <div className="mb-3 flex items-start gap-2 rounded bg-muted/50 px-2 py-1.5 text-xs text-muted-foreground">
            <Info className="h-3.5 w-3.5 mt-0.5 shrink-0" />
            <span>
              No <code className="rounded bg-muted px-1">pixi.toml</code> found.
              Run <code className="rounded bg-muted px-1">pixi init</code> to
              create a pixi project.
            </span>
          </div>
        )}

        {/* Tip */}
        <div className="flex items-start gap-2 rounded bg-muted/50 px-2 py-1.5 text-xs text-muted-foreground">
          <Terminal className="h-3.5 w-3.5 mt-0.5 shrink-0" />
          <span>
            Manage dependencies with{" "}
            <code className="rounded bg-muted px-1">
              pixi add &lt;package&gt;
            </code>{" "}
            in your terminal.
          </span>
        </div>
      </div>
    </div>
  );
}
