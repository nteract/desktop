import { ChevronDown, ChevronRight } from "lucide-react";
import {
  type ReactNode,
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
} from "react";
import {
  CommBridgeManager,
  type IframeToParentMessage,
  IsolatedFrame,
  type IsolatedFrameHandle,
} from "@/components/isolated";
import {
  AnsiErrorOutput,
  AnsiStreamOutput,
} from "@/components/outputs/ansi-output";
import {
  DEFAULT_PRIORITY,
  MediaRouter,
} from "@/components/outputs/media-router";
import { useWidgetStore } from "@/components/widgets/widget-store-context";
import { isDarkMode as detectDarkMode } from "@/lib/dark-mode";
import { ErrorBoundary } from "@/lib/error-boundary";
import { highlightTextInDom } from "@/lib/highlight-text";
import { OutputErrorFallback } from "@/lib/output-error-fallback";
import { cn } from "@/lib/utils";

/**
 * Jupyter output types based on the nbformat spec.
 */
export type JupyterOutput =
  | {
      output_type: "execute_result" | "display_data";
      data: Record<string, unknown>;
      metadata?: Record<string, unknown>;
      execution_count?: number | null;
    }
  | {
      output_type: "stream";
      name: "stdout" | "stderr";
      text: string | string[];
    }
  | {
      output_type: "error";
      ename: string;
      evalue: string;
      traceback: string[];
    };

interface OutputAreaProps {
  /**
   * Array of Jupyter outputs to render.
   */
  outputs: JupyterOutput[];
  /**
   * Whether the output area is collapsed.
   */
  collapsed?: boolean;
  /**
   * Callback when collapse state is toggled.
   */
  onToggleCollapse?: () => void;
  /**
   * Maximum height before scrolling. Set to enable scroll behavior.
   */
  maxHeight?: number;
  /**
   * Additional CSS classes for the container.
   */
  className?: string;
  /**
   * Custom renderers passed to MediaRouter.
   */
  renderers?: Record<
    string,
    (props: {
      data: unknown;
      metadata: Record<string, unknown>;
      mimeType: string;
      className?: string;
    }) => ReactNode
  >;
  /**
   * Custom MIME type priority order.
   */
  priority?: readonly string[];
  /**
   * Force isolation mode. When true, all outputs render in an isolated iframe.
   * When "auto" (default), isolation is used when any output needs it.
   * When false, outputs render in-DOM (less secure but faster for simple outputs).
   */
  isolated?: boolean | "auto";
  /**
   * Pre-create the IsolatedFrame even when there are no outputs.
   * This allows the iframe to bootstrap ahead of time, making output rendering instant.
   * The iframe is hidden until outputs that need isolation arrive.
   * @default false
   */
  preloadIframe?: boolean;
  /**
   * Callback when a link is clicked in isolated outputs.
   */
  onLinkClick?: (url: string, newTab: boolean) => void;
  /**
   * Callback when widget state is updated in isolated outputs.
   * @deprecated Use the comm bridge instead for full widget support
   */
  onWidgetUpdate?: (commId: string, state: Record<string, unknown>) => void;
  /**
   * Search query to highlight in iframe outputs.
   * Empty string or undefined clears highlights.
   */
  searchQuery?: string;
}

/**
 * Normalize stream text (can be string or string array).
 */
function normalizeText(text: string | string[]): string {
  return Array.isArray(text) ? text.join("") : text;
}

/**
 * MIME types that require iframe isolation for security.
 * These types can contain executable scripts or interactive content.
 */
const ISOLATED_MIME_TYPES = new Set([
  "text/html",
  "text/markdown",
  "image/svg+xml",
  "application/vnd.jupyter.widget-view+json",
  "application/vnd.plotly.v1+json",
  "application/vnd.vegalite.v5+json",
  "application/vnd.vegalite.v4+json",
  "application/vnd.vegalite.v3+json",
  "application/vnd.vega.v5+json",
  "application/vnd.vega.v4+json",
]);

/**
 * Select the best MIME type from available data based on priority.
 */
function selectMimeType(
  data: Record<string, unknown>,
  priority: readonly string[] = DEFAULT_PRIORITY,
): string | null {
  const availableTypes = Object.keys(data);
  for (const mimeType of priority) {
    if (availableTypes.includes(mimeType) && data[mimeType] != null) {
      return mimeType;
    }
  }
  const firstAvailable = availableTypes.find((type) => data[type] != null);
  return firstAvailable || null;
}

/**
 * Check if a single output needs iframe isolation.
 */
function outputNeedsIsolation(
  output: JupyterOutput,
  priority: readonly string[] = DEFAULT_PRIORITY,
): boolean {
  if (
    output.output_type === "execute_result" ||
    output.output_type === "display_data"
  ) {
    const mimeType = selectMimeType(output.data, priority);
    return mimeType ? ISOLATED_MIME_TYPES.has(mimeType) : false;
  }
  // stream and error outputs don't need isolation
  return false;
}

/**
 * Check if any outputs in the array need iframe isolation.
 * If any output needs isolation, ALL outputs should go to the iframe.
 */
function anyOutputNeedsIsolation(
  outputs: JupyterOutput[],
  priority: readonly string[] = DEFAULT_PRIORITY,
): boolean {
  return outputs.some((output) => outputNeedsIsolation(output, priority));
}

/**
 * Check if outputs contain any widget MIME types.
 */
function hasWidgetOutputs(
  outputs: JupyterOutput[],
  priority: readonly string[] = DEFAULT_PRIORITY,
): boolean {
  return outputs.some((output) => {
    if (
      output.output_type === "execute_result" ||
      output.output_type === "display_data"
    ) {
      const mimeType = selectMimeType(output.data, priority);
      return mimeType === "application/vnd.jupyter.widget-view+json";
    }
    return false;
  });
}

/**
 * Render a single Jupyter output based on its type.
 */
function renderOutput(
  output: JupyterOutput,
  index: number,
  renderers?: OutputAreaProps["renderers"],
  priority?: readonly string[],
) {
  const key = `output-${index}`;

  switch (output.output_type) {
    case "execute_result":
    case "display_data":
      return (
        <MediaRouter
          key={key}
          data={output.data}
          metadata={
            output.metadata as Record<
              string,
              Record<string, unknown> | undefined
            >
          }
          renderers={renderers}
          priority={priority}
        />
      );

    case "stream":
      return (
        <AnsiStreamOutput
          key={key}
          text={normalizeText(output.text)}
          streamName={output.name}
        />
      );

    case "error":
      return (
        <AnsiErrorOutput
          key={key}
          ename={output.ename}
          evalue={output.evalue}
          traceback={output.traceback}
        />
      );

    default:
      return null;
  }
}

/**
 * OutputArea renders multiple Jupyter outputs with proper layout.
 *
 * Handles all Jupyter output types: execute_result, display_data, stream, and error.
 * Supports collapsible state and scroll behavior for large outputs.
 *
 * @example
 * ```tsx
 * <OutputArea
 *   outputs={cell.outputs}
 *   collapsed={outputsCollapsed}
 *   onToggleCollapse={() => setOutputsCollapsed(!outputsCollapsed)}
 *   maxHeight={400}
 * />
 * ```
 */
export function OutputArea({
  outputs,
  collapsed = false,
  onToggleCollapse,
  maxHeight,
  className,
  renderers,
  priority = DEFAULT_PRIORITY,
  isolated = "auto",
  preloadIframe = false,
  onLinkClick,
  onWidgetUpdate,
  searchQuery,
}: OutputAreaProps) {
  const id = useId();
  const frameRef = useRef<IsolatedFrameHandle>(null);
  const bridgeRef = useRef<CommBridgeManager | null>(null);
  const inDomOutputRef = useRef<HTMLDivElement>(null);

  // Track dark mode state and observe changes
  const [darkMode, setDarkMode] = useState(() => detectDarkMode());

  useEffect(() => {
    // Update dark mode when document class changes
    const observer = new MutationObserver(() => {
      setDarkMode(detectDarkMode());
    });

    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["class", "data-theme", "data-mode"],
    });

    return () => observer.disconnect();
  }, []);

  // Get widget store context (may be null if not in provider)
  const widgetContext = useWidgetStore();

  // Determine if we should use isolation (when we have outputs)
  const shouldIsolate =
    outputs.length > 0 &&
    (isolated === true ||
      (isolated === "auto" && anyOutputNeedsIsolation(outputs, priority)));

  // When preloading, we render the iframe even with no outputs (hidden)
  // This allows it to bootstrap ahead of time for instant rendering
  const showPreloadedIframe = preloadIframe && !collapsed;

  // Check if we have widgets and should set up comm bridge
  const hasWidgets = hasWidgetOutputs(outputs, priority);
  const shouldUseBridge = shouldIsolate && hasWidgets && widgetContext !== null;

  const hasCollapseControl = onToggleCollapse !== undefined;
  const outputCount = outputs.length;

  // Handle messages from iframe, routing widget messages to comm bridge
  const handleIframeMessage = useCallback(
    (message: IframeToParentMessage) => {
      // Route widget messages to bridge
      if (bridgeRef.current) {
        bridgeRef.current.handleIframeMessage(message);
      }

      // Also handle widget_update for backward compatibility
      if (message.type === "widget_update" && onWidgetUpdate) {
        onWidgetUpdate(message.payload.commId, message.payload.state);
      }
    },
    [onWidgetUpdate],
  );

  // Callback when frame is ready - set up bridge and render outputs
  const handleFrameReady = useCallback(() => {
    if (!frameRef.current) return;

    // Set up comm bridge if we have widgets and widget context
    if (shouldUseBridge && widgetContext && !bridgeRef.current) {
      bridgeRef.current = new CommBridgeManager({
        frame: frameRef.current,
        store: widgetContext.store,
        sendUpdate: widgetContext.sendUpdate,
        sendCustom: widgetContext.sendCustom,
        closeComm: widgetContext.closeComm,
      });
    }

    // Clear existing content
    frameRef.current.clear();

    // Render each output
    outputs.forEach((output, index) => {
      const append = index > 0;

      if (
        output.output_type === "execute_result" ||
        output.output_type === "display_data"
      ) {
        const mimeType = selectMimeType(output.data, priority);
        if (mimeType) {
          frameRef.current?.render({
            mimeType,
            data: output.data[mimeType],
            metadata: output.metadata?.[mimeType] as
              | Record<string, unknown>
              | undefined,
            outputIndex: index,
            append,
          });
        }
      } else if (output.output_type === "stream") {
        frameRef.current?.render({
          mimeType: "text/plain",
          data: normalizeText(output.text),
          metadata: { streamName: output.name },
          outputIndex: index,
          append,
        });
      } else if (output.output_type === "error") {
        // Render error with metadata so iframe can use AnsiErrorOutput
        frameRef.current?.render({
          mimeType: "text/plain",
          data: output.traceback.join("\n"),
          metadata: {
            isError: true,
            ename: output.ename,
            evalue: output.evalue,
            traceback: output.traceback,
          },
          outputIndex: index,
          append,
        });
      }
    });
  }, [outputs, priority, shouldUseBridge, widgetContext]);

  // Clean up bridge on unmount
  useEffect(() => {
    return () => {
      if (bridgeRef.current) {
        bridgeRef.current.dispose();
        bridgeRef.current = null;
      }
    };
  }, []);

  // Re-render outputs when they change (after initial ready)
  useEffect(() => {
    if (frameRef.current?.isReady) {
      handleFrameReady();
    }
  }, [handleFrameReady]);

  // Forward search query to the iframe (for isolated outputs)
  useEffect(() => {
    if (frameRef.current?.isIframeReady) {
      frameRef.current.search(searchQuery || "");
    }
  }, [searchQuery]);

  // Highlight search matches in in-DOM outputs
  // Re-run when outputs change (outputCount tracks this) so new content gets highlighted
  useEffect(() => {
    if (!inDomOutputRef.current || shouldIsolate || outputCount === 0) return;
    return highlightTextInDom(inDomOutputRef.current, searchQuery || "");
  }, [searchQuery, shouldIsolate, outputCount]);

  // Empty state: render nothing (unless preloading iframe)
  if (outputs.length === 0 && !showPreloadedIframe) {
    return null;
  }

  // Hide the entire output area when only preloading (no visible outputs)
  const isPreloadOnly = showPreloadedIframe && outputs.length === 0;

  return (
    <div
      data-slot="output-area"
      className={cn("output-area", isPreloadOnly && "hidden", className)}
    >
      {/* Collapse toggle */}
      {hasCollapseControl && (
        <button
          type="button"
          onClick={onToggleCollapse}
          className="flex items-center gap-1 px-2 py-1 text-xs text-muted-foreground hover:text-foreground transition-colors"
          aria-expanded={!collapsed}
          aria-controls={id}
        >
          {collapsed ? (
            <ChevronRight className="h-3 w-3" />
          ) : (
            <ChevronDown className="h-3 w-3" />
          )}
          <span>
            {collapsed
              ? `Show ${outputCount} output${outputCount > 1 ? "s" : ""}`
              : "Hide outputs"}
          </span>
        </button>
      )}

      {/* Output content */}
      {!collapsed && (
        <div
          id={id}
          className={cn("space-y-2", maxHeight && "overflow-y-auto")}
          style={maxHeight ? { maxHeight: `${maxHeight}px` } : undefined}
        >
          {/* Preloaded or active isolated frame */}
          {(shouldIsolate || showPreloadedIframe) && (
            <div className={shouldIsolate ? undefined : "hidden"}>
              <IsolatedFrame
                ref={frameRef}
                darkMode={darkMode}
                minHeight={24}
                maxHeight={maxHeight ?? 2000}
                onReady={handleFrameReady}
                onLinkClick={onLinkClick}
                onWidgetUpdate={onWidgetUpdate}
                onMessage={handleIframeMessage}
                onError={(err) =>
                  console.error("[OutputArea] iframe error:", err)
                }
              />
            </div>
          )}

          {/* In-DOM outputs (when not using isolation) */}
          {!shouldIsolate && (
            <div ref={inDomOutputRef}>
              {outputs.map((output, index) => (
                <div
                  key={`output-${index}`}
                  data-slot="output-item"
                  data-output-index={index}
                >
                  <ErrorBoundary
                    resetKeys={[JSON.stringify(output)]}
                    fallback={(error, reset) => (
                      <OutputErrorFallback
                        error={error}
                        outputIndex={index}
                        onRetry={reset}
                      />
                    )}
                    onError={(error, errorInfo) => {
                      console.error(
                        `[OutputArea] Error rendering output ${index}:`,
                        error,
                        errorInfo.componentStack,
                      );
                    }}
                  >
                    {renderOutput(output, index, renderers, priority)}
                  </ErrorBoundary>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
