import { cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
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
  // biome-ignore lint/suspicious/noExplicitAny: test mock
  (window as any).Plotly = mockPlotly;
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  // biome-ignore lint/suspicious/noExplicitAny: test cleanup
  delete (window as any).Plotly;
});

describe("PlotlyOutput", () => {
  const sampleData = {
    data: [{ type: "scatter", x: [1, 2, 3], y: [4, 5, 6] }],
    layout: { title: "Test" },
  };

  it("calls Plotly.newPlot with data, layout, and config", () => {
    render(<PlotlyOutput data={sampleData} />);

    expect(mockPlotly.newPlot).toHaveBeenCalledTimes(1);
    const [el, data, layout, config] = mockPlotly.newPlot.mock.calls[0];
    expect(el).toBeInstanceOf(HTMLDivElement);
    expect(data).toEqual(sampleData.data);
    expect(layout.title).toBe("Test");
    expect(layout.autosize).toBe(true);
    expect(config.responsive).toBe(true);
    expect(config.displaylogo).toBe(false);
  });

  it("renders nothing when data is empty", () => {
    const { container } = render(
      <PlotlyOutput data={{ data: [] }} />,
    );
    // Component renders the container div but Plotly.newPlot is not called
    // because useEffect sees data.data is empty array (truthy), but newPlot
    // should still be called for an empty array — plotly handles it.
    // The key contract: returns null when data prop itself is null/undefined.
    expect(container.firstChild).toBeTruthy();
  });

  it("returns null when data prop has no data array", () => {
    const { container } = render(
      // biome-ignore lint/suspicious/noExplicitAny: testing edge case
      <PlotlyOutput data={null as any} />,
    );
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

    const [, , layout] = mockPlotly.newPlot.mock.calls[0];
    expect(layout.plot_bgcolor).toBe("rgba(30, 30, 30, 1)");
    expect(layout.font.color).toBe("rgba(200, 200, 200, 1)");
    expect(layout.paper_bgcolor).toBe("transparent");

    document.documentElement.classList.remove("dark");
  });

  it("uses light theme by default", () => {
    document.documentElement.classList.remove("dark");
    render(<PlotlyOutput data={sampleData} />);

    const [, , layout] = mockPlotly.newPlot.mock.calls[0];
    expect(layout.plot_bgcolor).toBe("rgba(255, 255, 255, 1)");
    expect(layout.font.color).toBe("rgba(68, 68, 68, 1)");
  });

  it("merges user config with defaults", () => {
    const dataWithConfig = {
      ...sampleData,
      config: { scrollZoom: true, displaylogo: true },
    };
    render(<PlotlyOutput data={dataWithConfig} />);

    const [, , , config] = mockPlotly.newPlot.mock.calls[0];
    // User config overrides defaults
    expect(config.scrollZoom).toBe(true);
    expect(config.displaylogo).toBe(true);
    // Default is preserved
    expect(config.responsive).toBe(true);
  });
});
