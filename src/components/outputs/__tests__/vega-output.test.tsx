import { cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
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
  mockVegaEmbed.mockResolvedValue({ view: { finalize: mockFinalize } });
  // biome-ignore lint/suspicious/noExplicitAny: test mock
  (window as any).vegaEmbed = mockVegaEmbed;
});

afterEach(() => {
  cleanup();
  // biome-ignore lint/suspicious/noExplicitAny: test cleanup
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
    expect(spec).toEqual(sampleVegaLiteSpec);
    expect(opts.actions).toBe(false);
    expect(opts.renderer).toBe("svg");
  });

  it("returns null when data is null", () => {
    const { container } = render(
      // biome-ignore lint/suspicious/noExplicitAny: testing edge case
      <VegaOutput data={null as any} />,
    );
    expect(container.firstChild).toBeNull();
  });

  it("calls view.finalize on unmount", async () => {
    const { unmount } = render(<VegaOutput data={sampleVegaLiteSpec} />);
    // Let the vegaEmbed promise resolve
    await vi.waitFor(() => expect(mockVegaEmbed).toHaveBeenCalled());
    // Flush microtasks so the .then() callback sets `view`
    await new Promise((r) => setTimeout(r, 0));
    unmount();
    expect(mockFinalize).toHaveBeenCalled();
  });

  it("applies dark theme colors when document has dark class", () => {
    document.documentElement.classList.add("dark");
    render(<VegaOutput data={sampleVegaLiteSpec} />);

    const [, , opts] = mockVegaEmbed.mock.calls[0];
    expect(opts.config.background).toBe("transparent");
    expect(opts.config.axis.labelColor).toBe("#ccc");
    expect(opts.config.legend.labelColor).toBe("#ccc");

    document.documentElement.classList.remove("dark");
  });

  it("applies light theme colors by default", () => {
    document.documentElement.classList.remove("dark");
    render(<VegaOutput data={sampleVegaLiteSpec} />);

    const [, , opts] = mockVegaEmbed.mock.calls[0];
    expect(opts.config.background).toBe("transparent");
    expect(opts.config.axis.labelColor).toBe("#333");
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
    expect(spec).toEqual(vegaSpec);
  });
});
