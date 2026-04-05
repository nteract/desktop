/**
 * Vega Renderer Plugin
 *
 * On-demand renderer plugin for Vega and Vega-Lite outputs. Bundles
 * vega-embed (+ vega, vega-lite as deps) directly — no window globals.
 * Loaded into the isolated iframe via the renderer plugin API.
 */

import { useEffect, useRef, useState } from "react";
import vegaEmbed from "vega-embed";
import { cn } from "@/lib/utils";

// --- Vega MIME detection (inlined to avoid importing from core bundle) ---

function isVegaMimeType(mime: string): boolean {
  return /^application\/vnd\.vega(lite)?\.v\d/.test(mime);
}

// No hardcoded MIME list — we use registerPattern with the regex matcher
// to handle any Vega/Vega-Lite version (v1, v2, ..., v6, future versions).

// --- VegaOutput component (self-contained, no window globals) ---

interface VegaView {
  finalize: () => void;
}

function embedOptions(isDark: boolean) {
  return {
    actions: false,
    renderer: "canvas" as const,
    theme: isDark ? ("dark" as const) : undefined,
  };
}

interface RendererProps {
  data: unknown;
  metadata?: Record<string, unknown>;
  mimeType: string;
}

function VegaRenderer({ data }: RendererProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setError(null);
    if (!containerRef.current || !data) return;

    const el = containerRef.current;
    const isDark = document.documentElement.classList.contains("dark");

    let view: VegaView | null = null;

    // Force transparent background so it blends with the cell.
    const spec = { ...(data as Record<string, unknown>), background: "transparent" };

    vegaEmbed(el, spec as never, embedOptions(isDark)).then(
      (result) => {
        view = result.view as VegaView;
      },
      (err: Error) => {
        console.error("[VegaRenderer] embed failed:", err);
        setError(err.message || String(err));
      },
    );

    // Re-embed on theme changes
    const themeObserver = new MutationObserver(() => {
      const nowDark = document.documentElement.classList.contains("dark");
      view?.finalize();
      vegaEmbed(el, spec as never, embedOptions(nowDark)).then(
        (result) => {
          view = result.view as VegaView;
        },
        (err: Error) => {
          console.error("[VegaRenderer] embed failed on theme change:", err);
        },
      );
    });
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["class"],
    });

    return () => {
      themeObserver.disconnect();
      view?.finalize();
    };
  }, [data]);

  if (!data) return null;

  return (
    <div
      ref={containerRef}
      data-slot="vega-output"
      className={cn("not-prose py-2 max-w-full overflow-visible")}
    >
      {error && (
        <div className="text-sm text-destructive py-1">
          Vega rendering error: {error}
        </div>
      )}
    </div>
  );
}

// --- Plugin install ---

export function install(ctx: {
  register: (
    mimeTypes: string[],
    component: React.ComponentType<RendererProps>,
  ) => void;
  registerPattern: (
    test: (mime: string) => boolean,
    component: React.ComponentType<RendererProps>,
  ) => void;
}) {
  // Use pattern matcher to handle any Vega/Vega-Lite version
  ctx.registerPattern(isVegaMimeType, VegaRenderer);
}

/**
 * Check if a MIME type is handled by this plugin.
 * Exported so iframe-libraries.ts can detect vega MIME types dynamically.
 */
export { isVegaMimeType };
