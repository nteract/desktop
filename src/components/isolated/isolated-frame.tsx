import { forwardRef, useCallback, useEffect, useImperativeHandle, useRef, useState } from "react";
import type { IframeToParentMessage, ParentToIframeMessage, RenderPayload } from "./frame-bridge";
import { isIframeMessage } from "./frame-bridge";
import { createFrameBlobUrl } from "./frame-html";
import { useIsolatedRenderer } from "./isolated-renderer-context";
import { JsonRpcTransport } from "./jsonrpc-transport";
import {
  NTERACT_BRIDGE_READY,
  NTERACT_CLEAR_OUTPUTS,
  NTERACT_COMM_CLOSE,
  NTERACT_COMM_MSG,
  NTERACT_COMM_OPEN,
  NTERACT_WIDGET_SNAPSHOT,
  NTERACT_DOUBLE_CLICK,
  NTERACT_ERROR,
  NTERACT_EVAL,
  NTERACT_EVAL_RESULT,
  NTERACT_INSTALL_RENDERER,
  NTERACT_LINK_CLICK,
  NTERACT_PING,
  NTERACT_RENDER_BATCH,
  NTERACT_RENDER_COMPLETE,
  NTERACT_RENDER_OUTPUT,
  NTERACT_RENDERER_READY,
  NTERACT_RESIZE,
  NTERACT_SEARCH,
  NTERACT_SEARCH_NAVIGATE,
  NTERACT_SEARCH_RESULTS,
  NTERACT_THEME,
  NTERACT_MOUSE_DOWN,
  NTERACT_WIDGET_COMM_CLOSE,
  NTERACT_WIDGET_COMM_MSG,
  NTERACT_WIDGET_READY,
  NTERACT_WIDGET_STATE,
  NTERACT_WIDGET_UPDATE,
} from "./rpc-methods";

export interface IsolatedFrameProps {
  /**
   * Unique ID for this frame (used for message routing).
   */
  id?: string;

  /**
   * Initial content to render when the frame is ready.
   */
  initialContent?: RenderPayload;

  /**
   * Whether to use dark mode styling.
   */
  darkMode?: boolean;

  /**
   * Color theme name (e.g., "classic", "cream").
   * Passed to the iframe as `data-color-theme` attribute.
   */
  colorTheme?: string;

  /**
   * Minimum height of the iframe in pixels.
   * @default 24
   */
  minHeight?: number;

  /**
   * Maximum height of the iframe in pixels.
   * Ignored when `autoHeight` is `true`.
   * @default 2000
   */
  maxHeight?: number;

  /**
   * When true, iframe grows to fit content without maxHeight cap.
   * Use for content that should render fully (e.g., markdown cells).
   * Takes precedence over maxHeight when enabled.
   * @default false
   */
  autoHeight?: boolean;

  /**
   * Additional CSS classes for the iframe container.
   */
  className?: string;

  /**
   * Callback when the iframe is ready to receive messages.
   */
  onReady?: () => void;

  /**
   * Callback when the iframe content resizes.
   */
  onResize?: (height: number) => void;

  /**
   * Callback when a link is clicked in the iframe.
   */
  onLinkClick?: (url: string, newTab: boolean) => void;

  /**
   * Callback when the user clicks (mousedown) inside the iframe.
   * Fires before any other click handling — does not interfere
   * with text selection, links, or widget interactions.
   */
  onMouseDown?: () => void;

  /**
   * Callback when the user double-clicks in the iframe.
   */
  onDoubleClick?: () => void;

  /**
   * Callback when a widget state update is sent from the iframe.
   */
  onWidgetUpdate?: (commId: string, state: Record<string, unknown>) => void;

  /**
   * Callback when an error occurs in the iframe.
   */
  onError?: (error: { message: string; stack?: string }) => void;

  /**
   * Callback for all messages from the iframe (for debugging or custom handling).
   */
  onMessage?: (message: IframeToParentMessage) => void;

  /**
   * When true, iframe starts hidden (0 height, 0 opacity) and reveals
   * with animation after content is rendered. Use for markdown cells
   * to prevent flash of empty/unstyled content during bootstrap.
   * @default false
   */
  revealOnRender?: boolean;
}

export interface IsolatedFrameHandle {
  /**
   * Send a message to the iframe.
   */
  send: (message: ParentToIframeMessage) => void;

  /**
   * Send content to render in the iframe.
   */
  render: (payload: RenderPayload) => void;

  /**
   * Atomically replace all outputs with a batch.
   * Uses stable IDs from cellId + outputIndex for smooth React reconciliation.
   */
  renderBatch: (outputs: RenderPayload[]) => void;

  /**
   * Evaluate code in the iframe (for bootstrap/injection).
   */
  eval: (code: string) => void;

  /**
   * Install a renderer plugin in the iframe.
   * The plugin is a CJS module that exports an install(ctx) function.
   * The iframe loads it with a custom require shim providing React,
   * then calls install() with a registration API for MIME types.
   */
  installRenderer: (code: string, css?: string) => void;

  /**
   * Update theme settings in the iframe.
   */
  setTheme: (isDark: boolean, colorTheme?: string | null) => void;

  /**
   * Clear all content in the iframe.
   */
  clear: () => void;

  /**
   * Search for text within the iframe's rendered content.
   * Pass empty string to clear search highlights.
   */
  search: (query: string, caseSensitive?: boolean) => void;

  /**
   * Navigate to a specific search match by index.
   */
  searchNavigate: (matchIndex: number) => void;

  /**
   * Whether the iframe is ready to receive messages.
   * True after the React renderer bundle is initialized.
   */
  isReady: boolean;

  /**
   * Whether the iframe bootstrap HTML is loaded.
   * True before the React renderer bundle is loaded.
   */
  isIframeReady: boolean;
}

const TYPE_TO_METHOD: Record<string, string> = {
  render: NTERACT_RENDER_OUTPUT,
  render_batch: NTERACT_RENDER_BATCH,
  theme: NTERACT_THEME,
  clear: NTERACT_CLEAR_OUTPUTS,
  eval: NTERACT_EVAL,
  install_renderer: NTERACT_INSTALL_RENDERER,
  ping: NTERACT_PING,
  search: NTERACT_SEARCH,
  search_navigate: NTERACT_SEARCH_NAVIGATE,
  comm_open: NTERACT_COMM_OPEN,
  comm_msg: NTERACT_COMM_MSG,
  comm_close: NTERACT_COMM_CLOSE,
  widget_snapshot: NTERACT_WIDGET_SNAPSHOT,
  bridge_ready: NTERACT_BRIDGE_READY,
  widget_state: NTERACT_WIDGET_STATE,
};

/**
 * Sandbox attributes for the isolated iframe.
 *
 * CRITICAL: Do NOT include 'allow-same-origin' - this would give the iframe
 * access to the parent's origin and Tauri APIs.
 */
const SANDBOX_ATTRS = [
  "allow-scripts", // Required for rendering interactive content
  "allow-downloads", // Allow file downloads (e.g., from widgets)
  "allow-forms", // Allow form submissions
  "allow-pointer-lock", // For interactive visualizations
  "allow-fullscreen", // For sift maximize, interactive visualizations (e.g., maps, 3D)
].join(" ");

/**
 * IsolatedFrame component - Renders untrusted content in a secure iframe.
 *
 * Uses a blob: URL with sandbox restrictions to ensure the iframe content
 * cannot access Tauri APIs or the parent DOM. Communication happens via
 * postMessage.
 *
 * **Requires** `IsolatedRendererProvider` to be present in the component tree.
 *
 * @example
 * ```tsx
 * // In your app root or layout:
 * <IsolatedRendererProvider basePath="/isolated">
 *   <App />
 * </IsolatedRendererProvider>
 *
 * // Then use IsolatedFrame anywhere:
 * const frameRef = useRef<IsolatedFrameHandle>(null);
 *
 * <IsolatedFrame
 *   ref={frameRef}
 *   darkMode={true}
 *   onReady={() => {
 *     frameRef.current?.render({
 *       mimeType: "text/html",
 *       data: "<h1>Hello from isolated frame!</h1>"
 *     });
 *   }}
 *   onResize={(height) => console.log("New height:", height)}
 * />
 * ```
 */
export const IsolatedFrame = forwardRef<IsolatedFrameHandle, IsolatedFrameProps>(
  function IsolatedFrame(
    {
      id,
      initialContent,
      darkMode = true,
      colorTheme,
      minHeight = 24,
      maxHeight = 2000,
      autoHeight = false,
      className = "",
      onReady,
      onResize,
      onLinkClick,
      onMouseDown,
      onDoubleClick,
      onWidgetUpdate,
      onError,
      onMessage,
      revealOnRender = false,
    },
    ref,
  ) {
    // Get renderer bundle from context (provided by IsolatedRendererProvider)
    const {
      rendererCode,
      rendererCss,
      isLoading: providerLoading,
      error: providerError,
    } = useIsolatedRenderer();
    const iframeRef = useRef<HTMLIFrameElement>(null);
    const rpcRef = useRef<JsonRpcTransport | null>(null);
    const [blobUrl, setBlobUrl] = useState<string | null>(null);
    // Track iframe ready (bootstrap HTML loaded)
    const [isIframeReady, setIsIframeReady] = useState(false);
    // Track renderer ready (React bundle initialized)
    const [isReady, setIsReady] = useState(false);
    // Use ref to track ready state for send callback (avoids stale closure)
    const isReadyRef = useRef(false);
    const [height, setHeight] = useState(minHeight);
    // Track if content has been rendered (for revealOnRender mode)
    const [isContentRendered, setIsContentRendered] = useState(false);
    // Track if the iframe is reloading (DOM move caused browser to tear down)
    // Used to hide the iframe during reload to prevent white flash
    const [isReloading, setIsReloading] = useState(false);

    // Queue messages until iframe is ready
    const pendingMessagesRef = useRef<ParentToIframeMessage[]>([]);
    // Track if we've started bootstrapping to avoid double-fetch
    const bootstrappingRef = useRef(false);
    // Track whether the iframe has sent a "ready" message before.
    // Any subsequent "ready" is a reload that needs the toggle trick.
    const hasReceivedReadyRef = useRef(false);

    // Track initial darkMode for blob URL (don't recreate blob on theme change)
    const initialDarkModeRef = useRef(darkMode);
    const initialColorThemeRef = useRef(colorTheme);

    // Stable refs for callback props — avoids tearing down the message
    // handler when callers pass unstable (inline) callbacks.
    const onReadyRef = useRef(onReady);
    const onResizeRef = useRef(onResize);
    const onLinkClickRef = useRef(onLinkClick);
    const onMouseDownRef = useRef(onMouseDown);
    const onDoubleClickRef = useRef(onDoubleClick);
    const onWidgetUpdateRef = useRef(onWidgetUpdate);
    const onErrorRef = useRef(onError);
    const onMessageRef = useRef(onMessage);

    // Sync refs during render so effects always see the latest callbacks.
    onReadyRef.current = onReady;
    onResizeRef.current = onResize;
    onLinkClickRef.current = onLinkClick;
    onMouseDownRef.current = onMouseDown;
    onDoubleClickRef.current = onDoubleClick;
    onWidgetUpdateRef.current = onWidgetUpdate;
    onErrorRef.current = onError;
    onMessageRef.current = onMessage;

    // Create blob URL on mount (only once, with initial darkMode)
    useEffect(() => {
      const url = createFrameBlobUrl({
        darkMode: initialDarkModeRef.current,
        colorTheme: initialColorThemeRef.current,
      });
      setBlobUrl(url);

      return () => {
        URL.revokeObjectURL(url);
      };
    }, []);

    // Send theme as soon as iframe is ready (before renderer bootstrap).
    // Once the renderer transport is active, prefer JSON-RPC so live theme
    // changes follow the same path as other renderer updates.
    useEffect(() => {
      if (isIframeReady && iframeRef.current?.contentWindow) {
        const payload = { isDark: darkMode, colorTheme: colorTheme ?? null };
        if (rpcRef.current && isReadyRef.current) {
          rpcRef.current.notify(NTERACT_THEME, payload);
        } else {
          iframeRef.current.contentWindow.postMessage({ type: "theme", payload }, "*");
        }
      }
    }, [darkMode, colorTheme, isIframeReady, isReady]);

    // Keep ref in sync with state (ref avoids stale closures in callbacks)
    useEffect(() => {
      isReadyRef.current = isReady;
    }, [isReady]);

    // Surface provider errors to consumers
    useEffect(() => {
      if (providerError && !providerLoading) {
        onErrorRef.current?.({
          message: providerError.message,
          stack: providerError.stack,
        });
      }
    }, [providerError, providerLoading]);

    // Send a message to the iframe
    // Uses ref to check ready state to avoid stale closure issues
    const send = useCallback(
      (message: ParentToIframeMessage) => {
        if (!isReadyRef.current) {
          // Queue message until ready
          pendingMessagesRef.current.push(message);
          return;
        }

        // Translate to JSON-RPC if transport is available
        const method = TYPE_TO_METHOD[message.type];
        if (method && rpcRef.current) {
          const params = "payload" in message ? message.payload : undefined;
          rpcRef.current.notify(method, params);
        } else if (iframeRef.current?.contentWindow) {
          // Fallback to legacy format
          iframeRef.current.contentWindow.postMessage(message, "*");
        }
      },
      [], // No deps - uses ref instead of state
    );

    // Flush pending messages when ready
    useEffect(() => {
      if (isReady && pendingMessagesRef.current.length > 0) {
        const pending = pendingMessagesRef.current;
        pendingMessagesRef.current = [];
        pending.forEach((msg) => {
          const method = TYPE_TO_METHOD[msg.type];
          if (method && rpcRef.current) {
            const params = "payload" in msg ? msg.payload : undefined;
            rpcRef.current.notify(method, params);
          } else if (iframeRef.current?.contentWindow) {
            iframeRef.current.contentWindow.postMessage(msg, "*");
          }
        });
      }
    }, [isReady]);

    // Handle messages from iframe
    useEffect(() => {
      const handleMessage = (event: MessageEvent) => {
        // Verify the message is from our iframe
        if (event.source !== iframeRef.current?.contentWindow) {
          return;
        }

        const data = event.data;

        // Skip JSON-RPC messages — the transport handles them
        if (
          typeof data === "object" &&
          data !== null &&
          (data as { jsonrpc?: unknown }).jsonrpc === "2.0"
        ) {
          return;
        }

        if (!isIframeMessage(data)) {
          return;
        }

        // Call generic message handler
        onMessageRef.current?.(data);

        // Handle specific message types
        switch (data.type) {
          case "ready": {
            // Iframe bootstrap HTML is loaded.
            // Any "ready" after the first is a reload (e.g., DOM move caused
            // the browser to tear down and reload the iframe).
            const isReload = hasReceivedReadyRef.current;
            hasReceivedReadyRef.current = true;

            if (isReload) {
              // Reset bootstrap state so the renderer gets re-injected.
              bootstrappingRef.current = false;
              // Keep the imperative readiness ref in sync with state so that
              // synchronous send() calls don't treat the frame as ready during
              // a reload window.
              isReadyRef.current = false;
              // Pending messages were targeted at the old iframe instance; drop
              // them so they don't get delivered to the reloaded frame.
              pendingMessagesRef.current.length = 0;
              setIsReady(false);
              // Reset content rendered state for revealOnRender mode
              setIsContentRendered(false);
              // Hide iframe during reload to prevent white flash from blank
              // iframe document before blob HTML loads
              setIsReloading(true);
            }

            if (isReload) {
              // Reload: isIframeReady may already be true, so toggle to
              // force effects that depend on it (theme sync, renderer
              // injection) to re-run.
              setIsIframeReady(false);
              setTimeout(() => {
                setIsIframeReady(true);
              }, 0);
            } else {
              // Initial load: a single transition from false→true is
              // sufficient.
              setIsIframeReady(true);
            }

            // Create JSON-RPC transport for this iframe instance
            if (iframeRef.current?.contentWindow) {
              // Clean up previous transport on reload
              if (rpcRef.current) {
                rpcRef.current.stop();
              }
              const transport = new JsonRpcTransport(
                iframeRef.current.contentWindow,
                iframeRef.current.contentWindow,
              );
              // Register handlers for JSON-RPC messages from iframe
              transport.onNotification(NTERACT_RENDERER_READY, () => {
                setIsReady(true);
                setIsReloading(false);
                onReadyRef.current?.();
                if (initialContent) {
                  transport.notify(NTERACT_RENDER_OUTPUT, initialContent);
                }
              });
              transport.onNotification(NTERACT_RESIZE, (params) => {
                const p = params as { height?: number };
                if (p.height != null) {
                  const newHeight = autoHeight
                    ? Math.max(minHeight, p.height)
                    : Math.max(minHeight, Math.min(maxHeight, p.height));
                  setHeight(newHeight);
                  onResizeRef.current?.(newHeight);
                }
              });
              transport.onNotification(NTERACT_RENDER_COMPLETE, (params) => {
                const p = params as { height?: number };
                if (p.height != null) {
                  setIsContentRendered(true);
                  const newHeight = autoHeight
                    ? Math.max(minHeight, p.height)
                    : Math.max(minHeight, Math.min(maxHeight, p.height));
                  setHeight(newHeight);
                  onResizeRef.current?.(newHeight);
                }
              });
              transport.onNotification(NTERACT_LINK_CLICK, (params) => {
                const p = params as { url: string; newTab?: boolean };
                if (p.url) {
                  onLinkClickRef.current?.(p.url, p.newTab ?? false);
                }
              });
              transport.onNotification(NTERACT_MOUSE_DOWN, () => {
                onMouseDownRef.current?.();
              });
              transport.onNotification(NTERACT_DOUBLE_CLICK, () => {
                onDoubleClickRef.current?.();
              });
              transport.onNotification(NTERACT_WIDGET_UPDATE, (params) => {
                const p = params as { commId: string; state: Record<string, unknown> };
                if (p.commId && p.state) {
                  onWidgetUpdateRef.current?.(p.commId, p.state);
                }
              });
              transport.onNotification(NTERACT_ERROR, (params) => {
                const p = params as { message: string; stack?: string };
                if (p.message) {
                  onErrorRef.current?.(p);
                }
              });
              transport.onNotification(NTERACT_EVAL_RESULT, (params) => {
                const p = params as { success: boolean; error?: string };
                if (p.success === false) {
                  console.error("[IsolatedFrame] Bundle eval failed:", p.error);
                  onErrorRef.current?.({ message: `Bundle eval failed: ${p.error}` });
                }
              });
              transport.onNotification(NTERACT_SEARCH_RESULTS, (params) => {
                const p = params as { count: number };
                // Forward to onMessage for OutputArea's search count tracking
                onMessageRef.current?.({
                  type: "search_results",
                  payload: p,
                } as IframeToParentMessage);
              });
              transport.onNotification(NTERACT_WIDGET_READY, () => {
                onMessageRef.current?.({ type: "widget_ready" } as IframeToParentMessage);
              });
              transport.onNotification(NTERACT_WIDGET_COMM_MSG, (params) => {
                onMessageRef.current?.({
                  type: "widget_comm_msg",
                  payload: params,
                } as IframeToParentMessage);
              });
              transport.onNotification(NTERACT_WIDGET_COMM_CLOSE, (params) => {
                onMessageRef.current?.({
                  type: "widget_comm_close",
                  payload: params,
                } as IframeToParentMessage);
              });
              transport.start();
              rpcRef.current = transport;
            }
            break;
          }

          case "renderer_ready":
            // React renderer bundle is initialized
            setIsReady(true);
            setIsReloading(false);
            onReadyRef.current?.();
            // Render initial content if provided
            if (initialContent) {
              iframeRef.current?.contentWindow?.postMessage(
                { type: "render", payload: initialContent },
                "*",
              );
            }
            break;

          case "resize":
            if (data.payload?.height != null) {
              const newHeight = autoHeight
                ? Math.max(minHeight, data.payload.height)
                : Math.max(minHeight, Math.min(maxHeight, data.payload.height));
              setHeight(newHeight);
              onResizeRef.current?.(newHeight);
            }
            break;

          case "render_complete":
            // Content has been rendered - reveal iframe if in revealOnRender mode
            if (data.payload?.height != null) {
              setIsContentRendered(true);
              const newHeight = autoHeight
                ? Math.max(minHeight, data.payload.height)
                : Math.max(minHeight, Math.min(maxHeight, data.payload.height));
              setHeight(newHeight);
              onResizeRef.current?.(newHeight);
            }
            break;

          case "link_click":
            if (data.payload?.url) {
              onLinkClickRef.current?.(data.payload.url, data.payload.newTab ?? false);
            }
            break;

          case "dblclick":
            onDoubleClickRef.current?.();
            break;

          case "widget_update":
            if (data.payload?.commId && data.payload?.state) {
              onWidgetUpdateRef.current?.(data.payload.commId, data.payload.state);
            }
            break;

          case "error":
            if (data.payload) {
              onErrorRef.current?.(data.payload);
            }
            break;

          case "eval_result":
            // Surface bundle eval failures to help diagnose injection issues
            if (data.payload?.success === false) {
              console.error("[IsolatedFrame] Bundle eval failed:", data.payload.error);
              onErrorRef.current?.({
                message: `Bundle eval failed: ${data.payload.error}`,
              });
            }
            break;
        }
      };

      window.addEventListener("message", handleMessage);
      return () => window.removeEventListener("message", handleMessage);
    }, [initialContent, minHeight, maxHeight, autoHeight]);

    // Clean up JSON-RPC transport on unmount
    useEffect(() => {
      return () => {
        rpcRef.current?.stop();
      };
    }, []);

    // Inject renderer when iframe is ready AND bundle props are available
    useEffect(() => {
      if (
        isIframeReady &&
        !isReady &&
        !bootstrappingRef.current &&
        rendererCode &&
        rendererCss &&
        iframeRef.current?.contentWindow
      ) {
        bootstrappingRef.current = true;

        // Inject CSS first (idempotent - checks if already loaded)
        const cssCode = `
        (function() {
          if (window.__ISOLATED_CSS_LOADED__) return;
          window.__ISOLATED_CSS_LOADED__ = true;
          var style = document.createElement('style');
          style.textContent = ${JSON.stringify(rendererCss)};
          document.head.appendChild(style);
        })();
      `;
        iframeRef.current.contentWindow.postMessage(
          { type: "eval", payload: { code: cssCode } },
          "*",
        );
        // Then inject JS bundle (idempotent - checks if already loaded)
        // Use string concatenation instead of template literal to avoid issues
        // with backticks or ${} in the bundled code
        const jsWrapper =
          "(function() {" +
          "if (window.__ISOLATED_RENDERER_LOADED__) return;" +
          "window.__ISOLATED_RENDERER_LOADED__ = true;" +
          rendererCode +
          "})();";
        iframeRef.current.contentWindow.postMessage(
          { type: "eval", payload: { code: jsWrapper } },
          "*",
        );
      }
    }, [isIframeReady, isReady, rendererCode, rendererCss]);

    // Expose imperative API
    useImperativeHandle(
      ref,
      () => ({
        send,
        render: (payload: RenderPayload) => send({ type: "render", payload }),
        renderBatch: (outputs: RenderPayload[]) =>
          send({ type: "render_batch", payload: { outputs } }),
        eval: (code: string) => send({ type: "eval", payload: { code } }),
        installRenderer: (code: string, css?: string) =>
          send({ type: "install_renderer", payload: { code, css } }),
        setTheme: (isDark: boolean, colorTheme?: string | null) =>
          send({ type: "theme", payload: { isDark, colorTheme } }),
        clear: () => send({ type: "clear" }),
        search: (query: string, caseSensitive?: boolean) => {
          // Search handler is in bootstrap HTML, so send directly when iframe is loaded
          // (bypasses the isReady queue which waits for the React renderer)
          if (rpcRef.current) {
            rpcRef.current.notify(NTERACT_SEARCH, { query, caseSensitive });
          } else if (iframeRef.current?.contentWindow) {
            iframeRef.current.contentWindow.postMessage(
              { type: "search", payload: { query, caseSensitive } },
              "*",
            );
          }
        },
        searchNavigate: (matchIndex: number) => {
          if (rpcRef.current) {
            rpcRef.current.notify(NTERACT_SEARCH_NAVIGATE, { matchIndex });
          } else if (iframeRef.current?.contentWindow) {
            iframeRef.current.contentWindow.postMessage(
              { type: "search_navigate", payload: { matchIndex } },
              "*",
            );
          }
        },
        isReady,
        isIframeReady,
      }),
      [send, isReady, isIframeReady],
    );

    if (!blobUrl) {
      return null;
    }

    // Compute display values for revealOnRender mode
    const displayHeight = revealOnRender && !isContentRendered ? 0 : height;
    const displayOpacity = revealOnRender && !isContentRendered ? 0 : 1;

    return (
      <iframe
        ref={iframeRef}
        id={id}
        src={blobUrl}
        sandbox={SANDBOX_ATTRS}
        allowFullScreen
        className={className}
        data-slot="isolated-frame"
        style={{
          width: "100%",
          height: `${displayHeight}px`,
          opacity: displayOpacity,
          border: "none",
          display: "block",
          background: "transparent",
          colorScheme: darkMode ? "dark" : "light",
          // Hide iframe during reload to prevent white flash from blank document.
          // visibility:hidden preserves layout (keeps height) while hiding content.
          visibility: isReloading ? "hidden" : "visible",
          transition: revealOnRender ? "height 150ms ease-out, opacity 150ms ease-out" : undefined,
        }}
        title=""
      />
    );
  },
);
