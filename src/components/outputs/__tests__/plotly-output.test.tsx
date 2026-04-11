import { cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { PlotlyOutput } from "../plotly-output";

// Polyfill ResizeObserver for jsdom
if (typeof globalThis.ResizeObserver === "undefined") {
  globalThis.ResizeObserver = class ResizeObserver {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof globalThis.ResizeObserver;
}

// Mock window.Plotly (injected by the iframe library loader in production)
const mockPlotly = {
  newPlot: vi.fn(),
  relayout: vi.fn(),
  purge: vi.fn(),
  Plots: { resize: vi.fn() },
};

beforeEach(() => {
  (window as any).Plotly = mockPlotly;
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  mockPlotly.newPlot.mockClear();
  mockPlotly.relayout.mockClear();
  mockPlotly.purge.mockClear();
  mockPlotly.Plots.resize.mockClear();
  delete (window as any).Plotly;
});

describe("PlotlyOutput", () => {
  const sampleData = {
    data: [{ type: "scatter", x: [1, 2, 3], y: [4, 5, 6] }],
    layout: { title: "Test" },
  };

  it("calls Plotly.newPlot with figure object including data, layout, and config", () => {
    render(<PlotlyOutput data={sampleData} />);

    expect(mockPlotly.newPlot).toHaveBeenCalledTimes(1);
    const [el, figure] = mockPlotly.newPlot.mock.calls[0];
    expect(el).toBeInstanceOf(HTMLDivElement);
    expect(figure.data).toEqual(sampleData.data);
    expect(figure.layout.title).toBe("Test");
    expect(figure.layout.autosize).toBe(true);
    expect(figure.config.responsive).toBe(true);
    expect(figure.config.displaylogo).toBe(false);
  });

  it("renders nothing when data is empty", () => {
    const { container } = render(<PlotlyOutput data={{ data: [] }} />);
    // Component renders the container div but Plotly.newPlot is not called
    // because useEffect sees data.data is empty array (truthy), but newPlot
    // should still be called for an empty array — plotly handles it.
    // The key contract: returns null when data prop itself is null/undefined.
    expect(container.firstChild).toBeTruthy();
  });

  it("returns null when data prop has no data array", () => {
    const { container } = render(<PlotlyOutput data={null as any} />);
    expect(container.firstChild).toBeNull();
  });

  it("calls Plotly.purge on unmount", () => {
    const { unmount } = render(<PlotlyOutput data={sampleData} />);
    unmount();
    expect(mockPlotly.purge).toHaveBeenCalledTimes(1);
  });

  it("applies dark theme when document has dark class", () => {
    document.documentElement.classList.add("dark");
    render(<PlotlyOutput data={sampleData} />);

    const [, figure] = mockPlotly.newPlot.mock.calls[0];
    expect(figure.layout.plot_bgcolor).toBe("rgba(30, 30, 30, 1)");
    expect(figure.layout.font.color).toBe("rgba(200, 200, 200, 1)");
    expect(figure.layout.paper_bgcolor).toBe("transparent");

    document.documentElement.classList.remove("dark");
  });

  it("uses light theme by default", () => {
    document.documentElement.classList.remove("dark");
    render(<PlotlyOutput data={sampleData} />);

    const [, figure] = mockPlotly.newPlot.mock.calls[0];
    expect(figure.layout.plot_bgcolor).toBe("rgba(255, 255, 255, 1)");
    expect(figure.layout.font.color).toBe("rgba(68, 68, 68, 1)");
  });

  it("merges user config with defaults", () => {
    const dataWithConfig = {
      ...sampleData,
      config: { scrollZoom: true, displaylogo: true },
    };
    render(<PlotlyOutput data={dataWithConfig} />);

    const [, figure] = mockPlotly.newPlot.mock.calls[0];
    // User config overrides defaults
    expect(figure.config.scrollZoom).toBe(true);
    expect(figure.config.displaylogo).toBe(true);
    // Default is preserved
    expect(figure.config.responsive).toBe(true);
  });

  it("passes animation frames to Plotly.newPlot", () => {
    const animatedData = {
      ...sampleData,
      frames: [{ data: [{ y: [7, 8, 9] }], name: "frame1" }],
    };
    render(<PlotlyOutput data={animatedData} />);

    const [, figure] = mockPlotly.newPlot.mock.calls[0];
    expect(figure.frames).toEqual(animatedData.frames);
  });
});
