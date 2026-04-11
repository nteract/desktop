import { cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { VegaOutput } from "../vega-output";

// Polyfill ResizeObserver for jsdom
if (typeof globalThis.ResizeObserver === "undefined") {
  globalThis.ResizeObserver = class ResizeObserver {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof globalThis.ResizeObserver;
}

// Mock window.vegaEmbed (injected by the iframe library loader in production)
const mockFinalize = vi.fn();
const mockVegaEmbed = vi.fn();

beforeEach(() => {
  mockFinalize.mockClear();
  mockVegaEmbed.mockClear();
  mockVegaEmbed.mockResolvedValue({
    view: { finalize: mockFinalize, background: vi.fn() },
  });
  (window as any).vegaEmbed = mockVegaEmbed;
});

afterEach(() => {
  cleanup();
  delete (window as any).vegaEmbed;
});

describe("VegaOutput", () => {
  const sampleVegaLiteSpec = {
    $schema: "https://vega.github.io/schema/vega-lite/v5.json",
    data: { values: [{ x: 1, y: 2 }] },
    mark: "point",
    encoding: {
      x: { field: "x", type: "quantitative" },
      y: { field: "y", type: "quantitative" },
    },
  };

  it("calls vegaEmbed with spec and options", () => {
    render(<VegaOutput data={sampleVegaLiteSpec} />);

    expect(mockVegaEmbed).toHaveBeenCalledTimes(1);
    const [el, spec, opts] = mockVegaEmbed.mock.calls[0];
    expect(el).toBeInstanceOf(HTMLDivElement);
    expect(spec.background).toBe("transparent");
    expect(opts.actions).toBe(false);
    expect(opts.renderer).toBe("canvas");
  });

  it("forces transparent background on the spec", () => {
    render(<VegaOutput data={sampleVegaLiteSpec} />);

    const [, spec] = mockVegaEmbed.mock.calls[0];
    expect(spec.background).toBe("transparent");
    // Original data should not be mutated
    expect(sampleVegaLiteSpec).not.toHaveProperty("background");
  });

  it("returns null when data is null", () => {
    const { container } = render(<VegaOutput data={null as any} />);
    expect(container.firstChild).toBeNull();
  });

  it("calls view.finalize on unmount", async () => {
    const { unmount } = render(<VegaOutput data={sampleVegaLiteSpec} />);
    await vi.waitFor(() => expect(mockVegaEmbed).toHaveBeenCalled());
    await new Promise((r) => setTimeout(r, 0));
    unmount();
    expect(mockFinalize).toHaveBeenCalled();
  });

  it("uses dark theme when document has dark class", () => {
    document.documentElement.classList.add("dark");
    render(<VegaOutput data={sampleVegaLiteSpec} />);

    const [, , opts] = mockVegaEmbed.mock.calls[0];
    expect(opts.theme).toBe("dark");

    document.documentElement.classList.remove("dark");
  });

  it("uses no theme by default (light)", () => {
    document.documentElement.classList.remove("dark");
    render(<VegaOutput data={sampleVegaLiteSpec} />);

    const [, , opts] = mockVegaEmbed.mock.calls[0];
    expect(opts.theme).toBeUndefined();
  });

  it("works with a Vega spec (not just Vega-Lite)", () => {
    const vegaSpec = {
      $schema: "https://vega.github.io/schema/vega/v5.json",
      width: 400,
      height: 200,
      data: [{ name: "table", values: [{ x: 1, y: 28 }] }],
    };
    render(<VegaOutput data={vegaSpec} />);

    expect(mockVegaEmbed).toHaveBeenCalledTimes(1);
    const [, spec] = mockVegaEmbed.mock.calls[0];
    expect(spec.background).toBe("transparent");
    expect(spec.$schema).toBe(vegaSpec.$schema);
  });
});
