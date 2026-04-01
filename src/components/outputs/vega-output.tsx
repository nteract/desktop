import { useEffect, useRef } from "react";
import { cn } from "@/lib/utils";

interface VegaOutputProps {
  data: Record<string, unknown>;
  className?: string;
}

function vegaEmbedOptions(isDark: boolean) {
  return {
    actions: false,
    renderer: "svg" as const,
    theme: isDark ? ("dark" as const) : undefined,
    config: {
      background: "transparent",
      ...(isDark
        ? {
            axis: {
              domainColor: "#666",
              gridColor: "#333",
              tickColor: "#666",
              labelColor: "#ccc",
              titleColor: "#ccc",
            },
            legend: { labelColor: "#ccc", titleColor: "#ccc" },
            title: { color: "#ccc" },
          }
        : {}),
    },
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

  useEffect(() => {
    // biome-ignore lint/suspicious/noExplicitAny: vega-embed is injected as a global
    const vegaEmbed = (window as any).vegaEmbed;
    if (!containerRef.current || !data || !vegaEmbed) return;

    const el = containerRef.current;
    const isDark = document.documentElement.classList.contains("dark");

    let view: { finalize: () => void } | null = null;

    vegaEmbed(el, data, vegaEmbedOptions(isDark)).then(
      (result: { view: { finalize: () => void } }) => {
        view = result.view;
      },
    );

    const themeObserver = new MutationObserver(() => {
      const nowDark = document.documentElement.classList.contains("dark");
      view?.finalize();
      vegaEmbed(el, data, vegaEmbedOptions(nowDark)).then(
        (result: { view: { finalize: () => void } }) => {
          view = result.view;
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
      className={cn("not-prose py-2 max-w-full", className)}
    />
  );
}
