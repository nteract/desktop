/**
 * Tests for media-router.tsx - MIME type selection and routing.
 *
 * These tests verify the MIME type selection logic that determines
 * which renderer to use for Jupyter output data, as well as component
 * rendering behavior for different MIME types.
 */

import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vite-plus/test";
import { MediaProvider } from "../media-provider";
import { DEFAULT_PRIORITY, getSelectedMimeType, MediaRouter } from "../media-router";

describe("getSelectedMimeType", () => {
  describe("priority-based selection", () => {
    it("returns highest priority MIME type when multiple available", () => {
      const data = {
        "text/plain": "Hello",
        "text/html": "<b>Hello</b>",
      };
      // text/html has higher priority than text/plain in DEFAULT_PRIORITY
      expect(getSelectedMimeType(data)).toBe("text/html");
    });

    it("returns widget MIME type over others", () => {
      const data = {
        "text/plain": "Widget fallback",
        "text/html": "<div>widget</div>",
        "application/vnd.jupyter.widget-view+json": { model_id: "abc" },
      };
      expect(getSelectedMimeType(data)).toBe("application/vnd.jupyter.widget-view+json");
    });

    it("returns image/png over text/plain", () => {
      const data = {
        "text/plain": "<Figure>",
        "image/png": "iVBORw0KGgo...",
      };
      expect(getSelectedMimeType(data)).toBe("image/png");
    });

    it("returns application/json over text/plain", () => {
      const data = {
        "text/plain": '{"key": "value"}',
        "application/json": { key: "value" },
      };
      expect(getSelectedMimeType(data)).toBe("application/json");
    });

    it("returns markdown over plain text", () => {
      const data = {
        "text/plain": "# Hello",
        "text/markdown": "# Hello",
      };
      expect(getSelectedMimeType(data)).toBe("text/markdown");
    });
  });

  describe("custom priority", () => {
    it("respects custom priority order", () => {
      const data = {
        "text/plain": "Hello",
        "text/html": "<b>Hello</b>",
        "application/json": { greeting: "Hello" },
      };
      // Custom priority puts text/plain first
      const customPriority = ["text/plain", "application/json", "text/html"];
      expect(getSelectedMimeType(data, customPriority)).toBe("text/plain");
    });

    it("falls back to first available when no priority match", () => {
      const data = {
        "custom/mime-type": "custom data",
      };
      // DEFAULT_PRIORITY doesn't include custom/mime-type
      expect(getSelectedMimeType(data)).toBe("custom/mime-type");
    });

    it("uses DEFAULT_PRIORITY when no custom priority provided", () => {
      const data = {
        "text/plain": "plain",
        "text/html": "html",
      };
      expect(getSelectedMimeType(data, DEFAULT_PRIORITY)).toBe("text/html");
    });
  });

  describe("empty/null handling", () => {
    it("returns null for empty data object", () => {
      expect(getSelectedMimeType({})).toBeNull();
    });

    it("skips null values", () => {
      const data = {
        "text/html": null,
        "text/plain": "fallback",
      };
      expect(getSelectedMimeType(data)).toBe("text/plain");
    });

    it("skips undefined values", () => {
      const data: Record<string, unknown> = {
        "text/html": undefined,
        "text/plain": "fallback",
      };
      expect(getSelectedMimeType(data)).toBe("text/plain");
    });

    it("returns null when all values are null", () => {
      const data = {
        "text/html": null,
        "text/plain": null,
      };
      expect(getSelectedMimeType(data)).toBeNull();
    });

    it("accepts empty string as valid value", () => {
      const data = {
        "text/plain": "",
      };
      // Empty string is falsy but not null/undefined
      expect(getSelectedMimeType(data)).toBe("text/plain");
    });

    it("accepts zero as valid value", () => {
      const data = {
        "application/json": 0,
      };
      expect(getSelectedMimeType(data)).toBe("application/json");
    });

    it("accepts false as valid value", () => {
      const data = {
        "application/json": false,
      };
      expect(getSelectedMimeType(data)).toBe("application/json");
    });
  });

  describe("various MIME types", () => {
    it("selects SVG over PNG", () => {
      const data = {
        "image/png": "png data",
        "image/svg+xml": "<svg>...</svg>",
      };
      expect(getSelectedMimeType(data)).toBe("image/svg+xml");
    });

    it("selects Plotly over HTML", () => {
      const data = {
        "text/html": "<div>plotly</div>",
        "application/vnd.plotly.v1+json": { data: [], layout: {} },
      };
      expect(getSelectedMimeType(data)).toBe("application/vnd.plotly.v1+json");
    });

    it("selects Vega-Lite v5 over v4", () => {
      const data = {
        "application/vnd.vegalite.v4+json": { $schema: "v4" },
        "application/vnd.vegalite.v5+json": { $schema: "v5" },
      };
      expect(getSelectedMimeType(data)).toBe("application/vnd.vegalite.v5+json");
    });

    it("handles GeoJSON", () => {
      const data = {
        "text/plain": "geojson",
        "application/geo+json": { type: "Feature" },
      };
      expect(getSelectedMimeType(data)).toBe("application/geo+json");
    });

    it("handles image/gif", () => {
      const data = {
        "text/plain": "<animation>",
        "image/gif": "R0lGODlh...",
      };
      expect(getSelectedMimeType(data)).toBe("image/gif");
    });

    it("handles image/webp", () => {
      const data = {
        "text/plain": "<image>",
        "image/webp": "UklGRl4A...",
      };
      expect(getSelectedMimeType(data)).toBe("image/webp");
    });

    it("handles image/jpeg", () => {
      const data = {
        "text/plain": "<image>",
        "image/jpeg": "/9j/4AAQ...",
      };
      expect(getSelectedMimeType(data)).toBe("image/jpeg");
    });
  });

  describe("DEFAULT_PRIORITY constant", () => {
    it("has widget as highest priority", () => {
      expect(DEFAULT_PRIORITY[0]).toBe("application/vnd.jupyter.widget-view+json");
    });

    it("has text/plain as lowest priority", () => {
      expect(DEFAULT_PRIORITY[DEFAULT_PRIORITY.length - 1]).toBe("text/plain");
    });

    it("includes all standard image types", () => {
      expect(DEFAULT_PRIORITY).toContain("image/png");
      expect(DEFAULT_PRIORITY).toContain("image/jpeg");
      expect(DEFAULT_PRIORITY).toContain("image/gif");
      expect(DEFAULT_PRIORITY).toContain("image/webp");
      expect(DEFAULT_PRIORITY).toContain("image/svg+xml");
    });

    it("includes rich visualization types", () => {
      expect(DEFAULT_PRIORITY).toContain("application/vnd.plotly.v1+json");
      expect(DEFAULT_PRIORITY).toContain("application/vnd.vegalite.v5+json");
      expect(DEFAULT_PRIORITY).toContain("application/vnd.vega.v5+json");
    });
  });
});

/**
 * Component tests for MediaRouter rendering behavior.
 */
describe("MediaRouter component", () => {
  describe("isolated MIME type handling", () => {
    // These MIME types are routed to IsolatedFrame by OutputArea.tsx
    // and should not be rendered by MediaRouter in the main bundle.
    // MediaRouter renders an empty wrapper (with data-slot) for these
    // to avoid rendering potentially unsafe content directly in the DOM.

    it("renders empty wrapper for text/html", () => {
      // Suppress the expected console.warn in dev mode
      const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

      const { container } = render(
        <MediaProvider>
          <MediaRouter data={{ "text/html": "<b>test</b>" }} />
        </MediaProvider>,
      );
      const wrapper = container.firstChild as HTMLElement;
      expect(wrapper).toHaveAttribute("data-slot", "media-router");
      expect(wrapper.children.length).toBe(0);

      warnSpy.mockRestore();
    });

    it("renders empty wrapper for text/markdown", () => {
      const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

      const { container } = render(
        <MediaProvider>
          <MediaRouter data={{ "text/markdown": "# Test" }} />
        </MediaProvider>,
      );
      const wrapper = container.firstChild as HTMLElement;
      expect(wrapper).toHaveAttribute("data-slot", "media-router");
      expect(wrapper.children.length).toBe(0);

      warnSpy.mockRestore();
    });

    it("renders empty wrapper for image/svg+xml", () => {
      const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

      const { container } = render(
        <MediaProvider>
          <MediaRouter data={{ "image/svg+xml": "<svg></svg>" }} />
        </MediaProvider>,
      );
      const wrapper = container.firstChild as HTMLElement;
      expect(wrapper).toHaveAttribute("data-slot", "media-router");
      expect(wrapper.children.length).toBe(0);

      warnSpy.mockRestore();
    });
  });

  describe("non-isolated rendering (lazy loaded)", () => {
    it("renders text/plain after lazy load", async () => {
      render(
        <MediaProvider>
          <MediaRouter data={{ "text/plain": "Hello World" }} />
        </MediaProvider>,
      );
      await waitFor(() => {
        expect(screen.getByText("Hello World")).toBeInTheDocument();
      });
    });

    it("renders images after lazy load", async () => {
      // 1x1 transparent PNG as base64
      const pngData =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

      render(
        <MediaProvider>
          <MediaRouter data={{ "image/png": pngData }} />
        </MediaProvider>,
      );
      await waitFor(() => {
        expect(screen.getByRole("img")).toBeInTheDocument();
      });
    });

    it("renders JSON after lazy load", async () => {
      render(
        <MediaProvider>
          <MediaRouter data={{ "application/json": { key: "value" } }} />
        </MediaProvider>,
      );
      // JsonOutput renders in a pre element with the JSON structure
      await waitFor(() => {
        expect(screen.getByText(/"key"/)).toBeInTheDocument();
      });
    });

    it("renders ANSI escape sequences correctly", async () => {
      // Red text using ANSI escape code
      const ansiText = "\x1b[31mRed text\x1b[0m";

      render(
        <MediaProvider>
          <MediaRouter data={{ "text/plain": ansiText }} />
        </MediaProvider>,
      );
      await waitFor(() => {
        expect(screen.getByText("Red text")).toBeInTheDocument();
      });
    });
  });

  describe("fallback behavior", () => {
    it("renders fallback when no data matches", () => {
      render(
        <MediaProvider>
          <MediaRouter data={{}} fallback={<div>No output</div>} />
        </MediaProvider>,
      );
      expect(screen.getByText("No output")).toBeInTheDocument();
    });

    it("renders default message when no data and no fallback", () => {
      render(
        <MediaProvider>
          <MediaRouter data={{}} />
        </MediaProvider>,
      );
      expect(screen.getByText("No displayable output")).toBeInTheDocument();
    });

    it("blocks unknown types in main DOM (requires iframe isolation)", async () => {
      render(
        <MediaProvider>
          <MediaRouter data={{ "application/octet-stream": "binary data" }} />
        </MediaProvider>,
      );
      // Unknown types need iframe isolation — render empty in main DOM
      await waitFor(() => {
        const container = screen.getByText((_content, element) => {
          return element?.getAttribute("data-mime-type") === "application/octet-stream";
        });
        expect(container).toBeInTheDocument();
      });
      expect(screen.queryByText("binary data")).not.toBeInTheDocument();
    });
  });

  describe("unknown text/* MIME types", () => {
    it("blocks unknown text/* types in main DOM (requires iframe isolation)", async () => {
      render(
        <MediaProvider>
          <MediaRouter data={{ "text/snazzy": "hello from custom type" }} />
        </MediaProvider>,
      );
      // Unknown text/* types need iframe isolation — render empty in main DOM
      await waitFor(() => {
        const container = screen.getByText((_content, element) => {
          return element?.getAttribute("data-mime-type") === "text/snazzy";
        });
        expect(container).toBeInTheDocument();
      });
      expect(screen.queryByText("hello from custom type")).not.toBeInTheDocument();
    });

    it("does NOT show a MIME type label for text/plain", async () => {
      render(
        <MediaProvider>
          <MediaRouter data={{ "text/plain": "regular plain text" }} />
        </MediaProvider>,
      );
      await waitFor(() => {
        expect(screen.getByText("regular plain text")).toBeInTheDocument();
      });
      expect(screen.queryByText("text/plain")).not.toBeInTheDocument();
    });
  });

  describe("custom renderers", () => {
    it("uses custom renderer when provided", () => {
      render(
        <MediaProvider>
          <MediaRouter
            data={{ "custom/type": "custom data" }}
            renderers={{
              "custom/type": ({ data }) => <div>Custom: {String(data)}</div>,
            }}
          />
        </MediaProvider>,
      );
      expect(screen.getByText("Custom: custom data")).toBeInTheDocument();
    });

    it("custom renderer takes priority over built-in", () => {
      render(
        <MediaProvider>
          <MediaRouter
            data={{ "text/plain": "plain text" }}
            renderers={{
              "text/plain": ({ data }) => <div>Overridden: {String(data)}</div>,
            }}
          />
        </MediaProvider>,
      );
      expect(screen.getByText("Overridden: plain text")).toBeInTheDocument();
    });
  });
});
