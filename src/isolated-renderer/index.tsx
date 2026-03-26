/**
 * Isolated Renderer Entry Point
 *
 * This module runs inside an isolated iframe and renders Jupyter outputs
 * using React and the existing output components. It receives render
 * commands from the parent via postMessage and displays them.
 *
 * Security: This code runs in a sandboxed iframe with an opaque origin.
 * It cannot access Tauri APIs, the parent DOM, or localStorage.
 */

import { StrictMode, useCallback, useEffect, useState } from "react";
import { createRoot, type Root } from "react-dom/client";

// Import styles (Tailwind + theme variables)
import "./styles.css";

import type { RenderPayload } from "@/components/isolated/frame-bridge";
// Import output components directly (not through MediaRouter's lazy loading)
// This ensures all components are bundled inline for the isolated iframe
import {
  AnsiErrorOutput,
  AnsiOutput,
  AnsiStreamOutput,
} from "@/components/outputs/ansi-output";
import { HtmlOutput } from "@/components/outputs/html-output";
import { ImageOutput } from "@/components/outputs/image-output";
import { JsonOutput } from "@/components/outputs/json-output";
import { MarkdownOutput } from "@/components/outputs/markdown-output";
import { SvgOutput } from "@/components/outputs/svg-output";
import { WidgetView } from "@/components/widgets/widget-view";
// Import widget support
import { IframeWidgetStoreProvider } from "./widget-provider";

// Import widget controls to register them in the widget registry
// This import has side effects that register all built-in widgets
import "@/components/widgets/controls";

// --- Types ---

interface OutputEntry {
  id: string;
  payload: RenderPayload;
}

interface RendererState {
  outputs: OutputEntry[];
  isDark: boolean;
}

// --- Theme Management ---

/**
 * Update the document theme so components can detect it via isDarkMode().
 * Sets class and data-theme on documentElement (html tag).
 */
function updateDocumentTheme(isDark: boolean) {
  const root = document.documentElement;

  // Set class for Tailwind dark: variant detection
  if (isDark) {
    root.classList.add("dark");
    root.classList.remove("light");
  } else {
    root.classList.add("light");
    root.classList.remove("dark");
  }

  // Set data-theme for components that check this attribute
  root.setAttribute("data-theme", isDark ? "dark" : "light");

  // Set color-scheme to influence prefers-color-scheme media queries
  // Some widgets (like drawdata) use @media (prefers-color-scheme: dark)
  root.style.colorScheme = isDark ? "dark" : "light";

  // Update CSS variables for base styles (background kept transparent for cell focus colors to show through)
  if (isDark) {
    root.style.setProperty("--bg-primary", "#0a0a0a");
    root.style.setProperty("--bg-secondary", "#1a1a1a");
    root.style.setProperty("--text-primary", "#e0e0e0");
    root.style.setProperty("--text-secondary", "#a0a0a0");
    root.style.setProperty("--foreground", "#e0e0e0");
  } else {
    root.style.setProperty("--bg-primary", "#ffffff");
    root.style.setProperty("--bg-secondary", "#f5f5f5");
    root.style.setProperty("--text-primary", "#1a1a1a");
    root.style.setProperty("--text-secondary", "#666666");
    root.style.setProperty("--foreground", "#1a1a1a");
  }
}

// --- Message Handling ---

type MessageHandler = (type: string, payload: unknown) => void;

let messageHandler: MessageHandler | null = null;

function setupMessageListener() {
  window.addEventListener("message", (event) => {
    // Only accept messages from parent
    if (event.source !== window.parent) return;

    const { type, payload } = event.data || {};
    if (messageHandler) {
      messageHandler(type, payload);
    }
  });
}

// --- React App ---

function IsolatedRendererApp() {
  const [state, setState] = useState<RendererState>({
    outputs: [],
    isDark: true,
  });

  // Handle messages from parent
  const handleMessage = useCallback((type: string, payload: unknown) => {
    switch (type) {
      case "render": {
        const renderPayload = payload as RenderPayload;

        // Generate stable ID when cellId is provided for better React reconciliation
        const id = renderPayload.cellId
          ? `${renderPayload.cellId}-${renderPayload.outputIndex ?? 0}`
          : `output-${Date.now()}-${Math.random().toString(36).slice(2)}`;

        setState((prev) => {
          if (renderPayload.replace) {
            // Replace all outputs with this single new output
            return { ...prev, outputs: [{ id, payload: renderPayload }] };
          }
          // Default: append to existing outputs
          return {
            ...prev,
            outputs: [...prev.outputs, { id, payload: renderPayload }],
          };
        });

        // Notify parent of render completion after next paint
        requestAnimationFrame(() => {
          window.parent.postMessage(
            {
              type: "render_complete",
              payload: { height: document.body.scrollHeight },
            },
            "*",
          );
        });
        break;
      }

      case "clear":
        setState((prev) => ({ ...prev, outputs: [] }));
        requestAnimationFrame(() => {
          window.parent.postMessage(
            {
              type: "render_complete",
              payload: { height: document.body.scrollHeight },
            },
            "*",
          );
        });
        break;

      case "theme": {
        const themePayload = payload as { isDark?: boolean };
        if (themePayload?.isDark !== undefined) {
          setState((prev) => ({ ...prev, isDark: themePayload.isDark! }));
          // Update theme on document.documentElement so theme detection works
          updateDocumentTheme(themePayload.isDark);
        }
        break;
      }
    }
  }, []);

  // Register message handler and notify parent when ready
  useEffect(() => {
    messageHandler = handleMessage;

    // Now that the handler is registered, notify parent that renderer is ready
    // This ensures messages won't be dropped due to race conditions
    window.parent.postMessage({ type: "renderer_ready" }, "*");

    return () => {
      messageHandler = null;
    };
  }, [handleMessage]);

  return (
    <div
      className="isolated-renderer"
      data-theme={state.isDark ? "dark" : "light"}
    >
      {state.outputs.map((entry) => (
        <OutputRenderer key={entry.id} payload={entry.payload} />
      ))}
    </div>
  );
}

/**
 * Render a single output based on its MIME type.
 * Uses direct component imports (not lazy loading) for isolated iframe compatibility.
 */
function OutputRenderer({ payload }: { payload: RenderPayload }) {
  const { mimeType, data, metadata } = payload;
  const content = data;

  // Handle stream output (plain text with potential ANSI)
  if (mimeType === "text/plain" && metadata?.streamName) {
    return (
      <AnsiStreamOutput
        text={String(data)}
        streamName={metadata.streamName as "stdout" | "stderr"}
      />
    );
  }

  // Handle error output
  if (mimeType === "text/plain" && metadata?.isError) {
    return (
      <AnsiErrorOutput
        ename={String(metadata.ename || "Error")}
        evalue={String(metadata.evalue || "")}
        traceback={
          Array.isArray(metadata.traceback)
            ? metadata.traceback.map(String)
            : [String(data)]
        }
      />
    );
  }

  // Route to appropriate component based on MIME type
  // (Direct rendering without MediaRouter's lazy loading)

  // Widget view - render interactive Jupyter widget
  if (mimeType === "application/vnd.jupyter.widget-view+json") {
    const widgetData = data as { model_id: string };
    return <WidgetView modelId={widgetData.model_id} />;
  }

  // Markdown
  if (mimeType === "text/markdown") {
    return <MarkdownOutput content={String(content)} />;
  }

  // HTML
  if (mimeType === "text/html") {
    return <HtmlOutput content={String(content)} />;
  }

  // SVG
  if (mimeType === "image/svg+xml") {
    return <SvgOutput data={String(content)} />;
  }

  // Images (PNG, JPEG, GIF, WebP)
  if (mimeType.startsWith("image/")) {
    return (
      <ImageOutput
        data={String(content)}
        mediaType={
          mimeType as "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        }
        width={metadata?.width as number | undefined}
        height={metadata?.height as number | undefined}
      />
    );
  }

  // JSON
  if (mimeType === "application/json") {
    const jsonData =
      typeof content === "string" ? JSON.parse(content) : content;
    return <JsonOutput data={jsonData} />;
  }

  // Plain text / ANSI
  if (mimeType === "text/plain") {
    return <AnsiOutput>{String(content)}</AnsiOutput>;
  }

  // Unknown text/* types — render with a MIME type label for distinction
  if (mimeType.startsWith("text/")) {
    return (
      <div>
        <span
          style={{
            display: "inline-block",
            marginBottom: "4px",
            padding: "1px 6px",
            fontSize: "10px",
            fontFamily: "monospace",
            color: "var(--text-secondary)",
            background: "var(--bg-secondary)",
            borderRadius: "4px",
          }}
        >
          {mimeType}
        </span>
        <AnsiOutput>{String(content)}</AnsiOutput>
      </div>
    );
  }

  // Fallback: render as plain text
  return (
    <pre style={{ whiteSpace: "pre-wrap", wordWrap: "break-word" }}>
      {typeof content === "string" ? content : JSON.stringify(content, null, 2)}
    </pre>
  );
}

// --- Bootstrap ---

let root: Root | null = null;

/**
 * Initialize the renderer. Called when the bundle is eval'd in the iframe.
 */
// Declare the global flag type for TypeScript
declare global {
  interface Window {
    __REACT_RENDERER_ACTIVE__?: boolean;
  }
}

export function init() {
  // Signal to the inline handler that React is taking over
  // This prevents the inline handler from processing render/theme/clear messages
  window.__REACT_RENDERER_ACTIVE__ = true;

  // Set up message listener
  setupMessageListener();

  // Theme is controlled by parent's theme message (sent when iframe is ready)
  // Don't set a default here to avoid flash when parent sends different theme

  // Create root element if needed
  let rootEl = document.getElementById("root");
  if (!rootEl) {
    rootEl = document.createElement("div");
    rootEl.id = "root";
    document.body.appendChild(rootEl);
  }

  // Create React root and render with widget provider
  root = createRoot(rootEl);
  root.render(
    <StrictMode>
      <IframeWidgetStoreProvider>
        <IsolatedRendererApp />
      </IframeWidgetStoreProvider>
    </StrictMode>,
  );

  // Set up resize observer
  // Use rAF to collapse multiple resize callbacks per frame into one
  // postMessage (avoids "ResizeObserver loop completed with undelivered
  // notifications" errors when many iframes resize simultaneously).
  let resizeRafPending = false;
  const resizeObserver = new ResizeObserver(() => {
    if (resizeRafPending) return;
    resizeRafPending = true;
    requestAnimationFrame(() => {
      resizeRafPending = false;
      window.parent.postMessage(
        { type: "resize", payload: { height: document.body.scrollHeight } },
        "*",
      );
    });
  });
  resizeObserver.observe(document.body);

  // Note: "renderer_ready" is sent from the React component's useEffect
  // to ensure the message handler is registered before parent sends messages
}

// Auto-init if this is the main module being eval'd
// The parent will send us via eval, so we auto-start
if (typeof window !== "undefined") {
  try {
    init();
  } catch (err) {
    const error = err instanceof Error ? err : new Error(String(err));
    console.error("[IsolatedRenderer] init() failed:", error);
    // Report error back to parent
    window.parent.postMessage(
      {
        type: "error",
        payload: {
          message: `Renderer init failed: ${error.message}`,
          stack: error.stack,
        },
      },
      "*",
    );
  }
}
