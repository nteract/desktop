import { useEffect, useRef, useState } from "react";
import { cn } from "@/lib/utils";

interface VegaOutputProps {
  data: Record<string, unknown>;
  className?: string;
}

interface VegaView {
  finalize: () => void;
  background: (color: string | null) => void;
}

function embedOptions(isDark: boolean) {
  return {
    actions: false,
    renderer: "canvas" as const,
    theme: isDark ? ("dark" as const) : undefined,
  };
}

/**
 * Render a Vega or Vega-Lite chart inside an isolated iframe.
 *
 * This component expects `window.vegaEmbed` to be available — it is injected
 * by the parent app via the iframe library loader before the render message
 * is sent. vega-embed auto-detects Vega vs Vega-Lite from the spec's $schema.
 */
export function VegaOutput({ data, className }: VegaOutputProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setError(null);

    const vegaEmbed = (window as any).vegaEmbed;
    if (!containerRef.current || !data || !vegaEmbed) return;

    const el = containerRef.current;
    const isDark = document.documentElement.classList.contains("dark");

    let view: VegaView | null = null;

    // Force transparent background on the spec so it blends with the cell.
    // Spec-level background has the highest priority in Vega's merge chain,
    // so this reliably overrides theme and config defaults.
    const spec = { ...data, background: "transparent" };

    vegaEmbed(el, spec, embedOptions(isDark)).then(
      (result: { view: VegaView }) => {
        view = result.view;
      },
      (err: Error) => {
        console.error("[VegaOutput] embed failed:", err);
        setError(err.message || String(err));
      },
    );

    const themeObserver = new MutationObserver(() => {
      const nowDark = document.documentElement.classList.contains("dark");
      view?.finalize();
      vegaEmbed(el, spec, embedOptions(nowDark)).then(
        (result: { view: VegaView }) => {
          view = result.view;
        },
        (err: Error) => {
          console.error("[VegaOutput] embed failed on theme change:", err);
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

  const vegaEmbed = (window as any).vegaEmbed;
  if (!vegaEmbed) {
    return (
      <div className="text-sm text-muted-foreground py-2">
        Vega library not loaded — chart cannot be rendered.
      </div>
    );
  }

  return (
    <div
      ref={containerRef}
      data-slot="vega-output"
      className={cn("not-prose py-2 max-w-full overflow-visible", className)}
    >
      {error && <div className="text-sm text-destructive py-1">Vega rendering error: {error}</div>}
    </div>
  );
}
