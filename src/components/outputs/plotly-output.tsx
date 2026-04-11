import { useEffect, useRef } from "react";
import { cn } from "@/lib/utils";

const DARK_TEXT = "rgba(200, 200, 200, 1)";
const LIGHT_TEXT = "rgba(68, 68, 68, 1)";

function darkLayoutOverrides(isDark: boolean): Record<string, unknown> {
  const textColor = isDark ? DARK_TEXT : LIGHT_TEXT;
  const gridColor = isDark ? "rgba(255, 255, 255, 0.1)" : "rgba(0, 0, 0, 0.1)";

  return {
    paper_bgcolor: "transparent",
    plot_bgcolor: isDark ? "rgba(30, 30, 30, 1)" : "rgba(255, 255, 255, 1)",
    font: { color: textColor },
    xaxis: { gridcolor: gridColor, zerolinecolor: gridColor, color: textColor },
    yaxis: { gridcolor: gridColor, zerolinecolor: gridColor, color: textColor },
    legend: { font: { color: textColor } },
    colorway: isDark
      ? [
          "#636efa",
          "#ef553b",
          "#00cc96",
          "#ab63fa",
          "#ffa15a",
          "#19d3f3",
          "#ff6692",
          "#b6e880",
          "#ff97ff",
          "#fecb52",
        ]
      : undefined,
  };
}

interface PlotlyData {
  data: unknown[];
  layout?: Record<string, unknown>;
  config?: Record<string, unknown>;
  frames?: unknown[];
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
    const Plotly = (window as any).Plotly;
    if (!containerRef.current || !data?.data || !Plotly) return;

    const el = containerRef.current;
    const isDark = document.documentElement.classList.contains("dark");

    const layout: Record<string, unknown> = {
      ...data.layout,
      ...darkLayoutOverrides(isDark),
      autosize: true,
    };

    const config: Record<string, unknown> = {
      responsive: true,
      displaylogo: false,
      modeBarButtonsToRemove: ["toImage"],
      ...data.config,
    };

    // Use the object form of newPlot so that animation frames are included.
    // The 4-arg form (el, data, layout, config) drops the frames key.
    Plotly.newPlot(el, { data: data.data, layout, config, frames: data.frames });

    const resizeObserver = new ResizeObserver(() => {
      Plotly.Plots.resize(el);
    });
    resizeObserver.observe(el);

    const themeObserver = new MutationObserver(() => {
      const nowDark = document.documentElement.classList.contains("dark");
      Plotly.relayout(el, darkLayoutOverrides(nowDark));
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
