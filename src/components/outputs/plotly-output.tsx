import { useEffect, useRef } from "react";
import { cn } from "@/lib/utils";

interface PlotlyData {
  data: unknown[];
  layout?: Record<string, unknown>;
  config?: Record<string, unknown>;
}

interface PlotlyOutputProps {
  data: PlotlyData;
  className?: string;
}

/**
 * Render a Plotly chart inside an isolated iframe.
 *
 * This component expects `window.Plotly` to be available — it is injected
 * by the parent app via the iframe library loader before the render message
 * is sent. It does NOT import plotly.js directly, keeping it out of the
 * isolated renderer IIFE bundle.
 */
export function PlotlyOutput({ data, className }: PlotlyOutputProps) {
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    // biome-ignore lint/suspicious/noExplicitAny: plotly.js is injected as a global
    const Plotly = (window as any).Plotly;
    if (!containerRef.current || !data?.data || !Plotly) return;

    const el = containerRef.current;
    const isDark = document.documentElement.classList.contains("dark");

    const layout: Record<string, unknown> = {
      ...data.layout,
      template: isDark ? "plotly_dark" : undefined,
      paper_bgcolor: "transparent",
      plot_bgcolor: isDark ? "rgba(30,30,30,1)" : "rgba(255,255,255,1)",
      autosize: true,
    };

    const config: Record<string, unknown> = {
      responsive: true,
      displaylogo: false,
      ...data.config,
    };

    Plotly.newPlot(el, data.data, layout, config);

    const resizeObserver = new ResizeObserver(() => {
      Plotly.Plots.resize(el);
    });
    resizeObserver.observe(el);

    const themeObserver = new MutationObserver(() => {
      const nowDark = document.documentElement.classList.contains("dark");
      Plotly.relayout(el, {
        template: nowDark ? "plotly_dark" : undefined,
        paper_bgcolor: "transparent",
        plot_bgcolor: nowDark ? "rgba(30,30,30,1)" : "rgba(255,255,255,1)",
      });
    });
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["class"],
    });

    return () => {
      resizeObserver.disconnect();
      themeObserver.disconnect();
      Plotly.purge(el);
    };
  }, [data]);

  if (!data?.data) return null;

  return (
    <div
      ref={containerRef}
      data-slot="plotly-output"
      className={cn("not-prose py-2 max-w-full", className)}
      style={{ minHeight: 400 }}
    />
  );
}
