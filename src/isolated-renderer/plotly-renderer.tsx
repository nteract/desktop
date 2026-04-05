/**
 * Plotly Renderer Plugin
 *
 * On-demand renderer plugin for application/vnd.plotly.v1+json outputs.
 * Bundles plotly.js directly — no window.Plotly global.
 * Loaded into the isolated iframe via the renderer plugin API.
 */

import Plotly from "plotly.js-dist-min";
import { useEffect, useRef } from "react";
import { cn } from "@/lib/utils";

// --- Theme helpers ---

const DARK_TEXT = "rgba(200, 200, 200, 1)";
const LIGHT_TEXT = "rgba(68, 68, 68, 1)";

function darkLayoutOverrides(isDark: boolean): Record<string, unknown> {
  const textColor = isDark ? DARK_TEXT : LIGHT_TEXT;
  const gridColor = isDark
    ? "rgba(255, 255, 255, 0.1)"
    : "rgba(0, 0, 0, 0.1)";

  return {
    paper_bgcolor: "transparent",
    plot_bgcolor: isDark ? "rgba(30, 30, 30, 1)" : "rgba(255, 255, 255, 1)",
    font: { color: textColor },
    xaxis: {
      gridcolor: gridColor,
      zerolinecolor: gridColor,
      color: textColor,
    },
    yaxis: {
      gridcolor: gridColor,
      zerolinecolor: gridColor,
      color: textColor,
    },
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

// --- Types ---

interface PlotlyData {
  data: unknown[];
  layout?: Record<string, unknown>;
  config?: Record<string, unknown>;
  frames?: unknown[];
}

interface RendererProps {
  data: unknown;
  metadata?: Record<string, unknown>;
  mimeType: string;
}

// --- PlotlyRenderer component ---

function PlotlyRenderer({ data: rawData }: RendererProps) {
  const containerRef = useRef<HTMLDivElement>(null);

  const data =
    typeof rawData === "string"
      ? (JSON.parse(rawData) as PlotlyData)
      : (rawData as PlotlyData);

  useEffect(() => {
    if (!containerRef.current || !data?.data) return;

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

    Plotly.newPlot(el, {
      data: data.data as Plotly.Data[],
      layout: layout as Partial<Plotly.Layout>,
      config: config as Partial<Plotly.Config>,
      frames: data.frames as Plotly.Frame[],
    });

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
      className={cn("not-prose py-2 max-w-full")}
      style={{ minHeight: 400 }}
    />
  );
}

// --- Plugin install ---

export function install(ctx: {
  register: (
    mimeTypes: string[],
    component: React.ComponentType<RendererProps>,
  ) => void;
}) {
  ctx.register(["application/vnd.plotly.v1+json"], PlotlyRenderer);
}
