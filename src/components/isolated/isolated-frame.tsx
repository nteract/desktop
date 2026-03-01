import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
} from "react";
import type {
  IframeToParentMessage,
  ParentToIframeMessage,
  RenderPayload,
} from "./frame-bridge";
import { isIframeMessage } from "./frame-bridge";
import { createFrameBlobUrl } from "./frame-html";
import { useIsolatedRenderer } from "./isolated-renderer-context";

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
   * Minimum height of the iframe in pixels.
   * @default 24
   */
  minHeight?: number;

  /**
   * Maximum height of the iframe in pixels.
   * @default 2000
   */
  maxHeight?: number;

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
   * Evaluate code in the iframe (for bootstrap/injection).
   */
  eval: (code: string) => void;

  /**
   * Update theme settings in the iframe.
   */
  setTheme: (isDark: boolean) => void;

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
  "allow-popups", // Allow window.open (for links)
  "allow-popups-to-escape-sandbox", // Popups should be unrestricted
  "allow-modals", // Allow alert/confirm/prompt
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
export const IsolatedFrame = forwardRef<
  IsolatedFrameHandle,
  IsolatedFrameProps
>(function IsolatedFrame(
  {
    id,
    initialContent,
    darkMode = true,
    minHeight = 24,
    maxHeight = 2000,
    className = "",
    onReady,
    onResize,
    onLinkClick,
    onDoubleClick,
    onWidgetUpdate,
    onError,
    onMessage,
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
  const [blobUrl, setBlobUrl] = useState<string | null>(null);
  // Track iframe ready (bootstrap HTML loaded)
  const [isIframeReady, setIsIframeReady] = useState(false);
  // Track renderer ready (React bundle initialized)
  const [isReady, setIsReady] = useState(false);
  // Use ref to track ready state for send callback (avoids stale closure)
  const isReadyRef = useRef(false);
  const [height, setHeight] = useState(minHeight);

  // Queue messages until iframe is ready
  const pendingMessagesRef = useRef<ParentToIframeMessage[]>([]);
  // Track if we've started bootstrapping to avoid double-fetch
  const bootstrappingRef = useRef(false);

  // Track initial darkMode for blob URL (don't recreate blob on theme change)
  const initialDarkModeRef = useRef(darkMode);

  // Create blob URL on mount (only once, with initial darkMode)
  useEffect(() => {
    const url = createFrameBlobUrl({ darkMode: initialDarkModeRef.current });
    setBlobUrl(url);

    return () => {
      URL.revokeObjectURL(url);
    };
  }, []);

  // Forward theme changes to iframe (without recreating the blob)
  useEffect(() => {
    if (isReady && iframeRef.current?.contentWindow) {
      iframeRef.current.contentWindow.postMessage(
        { type: "theme", payload: { isDark: darkMode } },
        "*",
      );
    }
  }, [darkMode, isReady]);

  // Keep ref in sync with state (ref avoids stale closures in callbacks)
  useEffect(() => {
    isReadyRef.current = isReady;
  }, [isReady]);

  // Surface provider errors to consumers
  useEffect(() => {
    if (providerError && !providerLoading) {
      onError?.({ message: providerError.message, stack: providerError.stack });
    }
  }, [providerError, providerLoading, onError]);

  // Send a message to the iframe
  // Uses ref to check ready state to avoid stale closure issues
  const send = useCallback(
    (message: ParentToIframeMessage) => {
      if (!isReadyRef.current) {
        // Queue message until ready
        pendingMessagesRef.current.push(message);
        return;
      }

      if (iframeRef.current?.contentWindow) {
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
        if (iframeRef.current?.contentWindow) {
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
      if (!isIframeMessage(data)) {
        return;
      }

      // Call generic message handler
      onMessage?.(data);

      // Handle specific message types
      switch (data.type) {
        case "ready":
          // Iframe bootstrap HTML is loaded
          // Renderer injection is handled by a separate useEffect
          setIsIframeReady(true);
          break;

        case "renderer_ready":
          // React renderer bundle is initialized
          setIsReady(true);
          onReady?.();
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
            const newHeight = Math.max(
              minHeight,
              Math.min(maxHeight, data.payload.height),
            );
            setHeight(newHeight);
            onResize?.(newHeight);
          }
          break;

        case "link_click":
          if (data.payload?.url) {
            onLinkClick?.(data.payload.url, data.payload.newTab ?? false);
          }
          break;

        case "dblclick":
          onDoubleClick?.();
          break;

        case "widget_update":
          if (data.payload?.commId && data.payload?.state) {
            onWidgetUpdate?.(data.payload.commId, data.payload.state);
          }
          break;

        case "error":
          if (data.payload) {
            onError?.(data.payload);
          }
          break;

        case "eval_result":
          // Surface bundle eval failures to help diagnose injection issues
          if (data.payload?.success === false) {
            console.error(
              "[IsolatedFrame] Bundle eval failed:",
              data.payload.error,
            );
            onError?.({ message: `Bundle eval failed: ${data.payload.error}` });
          }
          break;
      }
    };

    window.addEventListener("message", handleMessage);
    return () => window.removeEventListener("message", handleMessage);
  }, [
    initialContent,
    minHeight,
    maxHeight,
    onReady,
    onResize,
    onLinkClick,
    onDoubleClick,
    onWidgetUpdate,
    onError,
    onMessage,
  ]);

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
      eval: (code: string) => send({ type: "eval", payload: { code } }),
      setTheme: (isDark: boolean) =>
        send({ type: "theme", payload: { isDark } }),
      clear: () => send({ type: "clear" }),
      search: (query: string, caseSensitive?: boolean) => {
        // Search handler is in bootstrap HTML, so send directly when iframe is loaded
        // (bypasses the isReady queue which waits for the React renderer)
        if (iframeRef.current?.contentWindow) {
          iframeRef.current.contentWindow.postMessage(
            { type: "search", payload: { query, caseSensitive } },
            "*",
          );
        }
      },
      searchNavigate: (matchIndex: number) => {
        if (iframeRef.current?.contentWindow) {
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

  return (
    <iframe
      ref={iframeRef}
      id={id}
      src={blobUrl}
      sandbox={SANDBOX_ATTRS}
      className={className}
      data-slot="isolated-frame"
      style={{
        width: "100%",
        height: `${height}px`,
        border: "none",
        display: "block",
      }}
      title="Isolated output frame"
    />
  );
});
