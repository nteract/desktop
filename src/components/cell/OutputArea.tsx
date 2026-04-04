import { ChevronDown, ChevronRight } from "lucide-react";
import { type ReactNode, useCallback, useEffect, useId, useRef } from "react";
import {
  CommBridgeManager,
  type IframeToParentMessage,
  IsolatedFrame,
  type IsolatedFrameHandle,
} from "@/components/isolated";
import {
  getRequiredLibraries,
  injectLibraries,
} from "@/components/isolated/iframe-libraries";
import {
  AnsiErrorOutput,
  AnsiStreamOutput,
} from "@/components/outputs/ansi-output";
import { isSafeForMainDom } from "@/components/outputs/safe-mime-types";
import {
  DEFAULT_PRIORITY,
  MediaRouter,
} from "@/components/outputs/media-router";
import { useWidgetStore } from "@/components/widgets/widget-store-context";
import { useDarkMode } from "@/lib/dark-mode";
import { ErrorBoundary } from "@/lib/error-boundary";
import { highlightTextInDom } from "@/lib/highlight-text";
import { OutputErrorFallback } from "@/lib/output-error-fallback";
import { cn } from "@/lib/utils";

const handleIframeError = (err: { message: string; stack?: string }) =>
  console.error("[OutputArea] iframe error:", err);

import type { JupyterOutput } from "./jupyter-output";
// Re-export so existing imports continue to work.
export type { JupyterOutput } from "./jupyter-output";

interface OutputAreaProps {
  /**
   * Array of Jupyter outputs to render.
   */
  outputs: JupyterOutput[];
  /**
   * Cell ID for stable output keys in the iframe (enables smooth updates).
   */
  cellId?: string;
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
  /**
   * Callback reporting how many search matches were found in this cell's outputs.
   * Called when iframe reports search_results or in-DOM highlighting completes.
   */
  onSearchMatchCount?: (count: number) => void;
  /**
   * Callback when the user clicks (mousedown) inside an isolated output iframe.
   * Use to update cell focus when the click is captured by the iframe.
   */
  onIframeMouseDown?: () => void;
}

/**
 * Normalize stream text (can be string or string array).
 */
function normalizeText(text: string | string[]): string {
  return Array.isArray(text) ? text.join("") : text;
}

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
 * Uses the safe-list: anything not explicitly safe defaults to isolation.
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
    return mimeType ? !isSafeForMainDom(mimeType) : false;
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
  cellId,
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
  onSearchMatchCount,
  onIframeMouseDown,
}: OutputAreaProps) {
  const id = useId();
  const frameRef = useRef<IsolatedFrameHandle>(null);
  const bridgeRef = useRef<CommBridgeManager | null>(null);
  const inDomOutputRef = useRef<HTMLDivElement>(null);
  const injectedLibsRef = useRef(new Set<string>());
  const renderGenRef = useRef(0);
  const searchQueryRef = useRef(searchQuery);
  searchQueryRef.current = searchQuery;

  const darkMode = useDarkMode();
  // Ref for reading current darkMode in callbacks without adding to deps
  const darkModeRef = useRef(darkMode);
  darkModeRef.current = darkMode;

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

      // Capture search result count from iframe
      if (message.type === "search_results") {
        onSearchMatchCount?.(message.payload.count);
      }
    },
    [onWidgetUpdate, onSearchMatchCount],
  );

  // Callback when frame is ready - set up bridge and render outputs
  const handleFrameReady = useCallback(async () => {
    if (!frameRef.current) return;

    // Bump generation so any in-flight async handleFrameReady from a
    // previous outputs snapshot will bail out after it awaits.
    const gen = ++renderGenRef.current;

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

    // Ensure theme is in sync before re-rendering (fixes theme drift after cell moves)
    // Use ref to avoid adding darkMode to deps which would cause re-renders on theme toggle
    frameRef.current.setTheme(darkModeRef.current);

    // Inject any heavy libraries required by the outputs (e.g. plotly.js).
    // Must happen before clear+render so the eval messages arrive first.
    // Clear the tracking set on each call — a new or reloaded iframe won't
    // have the previously-injected globals, so we must re-inject.
    injectedLibsRef.current.clear();
    const neededLibs = getRequiredLibraries(
      outputs,
      (data) => selectMimeType(data, priority),
    );
    if (neededLibs.length > 0) {
      await injectLibraries(
        frameRef.current,
        neededLibs,
        injectedLibsRef.current,
      );
      // Stale check: if outputs changed while we were loading the library,
      // bail — a newer handleFrameReady call is already in flight.
      if (gen !== renderGenRef.current) return;
    }

    // Build batch of render payloads and send atomically.
    // This avoids the clear+re-render cycle that causes DOM thrashing
    // (visible as flickering when interactive widgets update rapidly).
    const batch: import("@/components/isolated/frame-bridge").RenderPayload[] =
      [];

    outputs.forEach((output, index) => {
      if (
        output.output_type === "execute_result" ||
        output.output_type === "display_data"
      ) {
        const mimeType = selectMimeType(output.data, priority);
        if (mimeType) {
          batch.push({
            mimeType,
            data: output.data[mimeType],
            metadata: output.metadata?.[mimeType] as
              | Record<string, unknown>
              | undefined,
            cellId,
            outputIndex: index,
          });
        }
      } else if (output.output_type === "stream") {
        batch.push({
          mimeType: "text/plain",
          data: normalizeText(output.text),
          metadata: { streamName: output.name },
          cellId,
          outputIndex: index,
        });
      } else if (output.output_type === "error") {
        batch.push({
          mimeType: "text/plain",
          data: output.traceback.join("\n"),
          metadata: {
            isError: true,
            ename: output.ename,
            evalue: output.evalue,
            traceback: output.traceback,
          },
          cellId,
          outputIndex: index,
        });
      }
    });

    frameRef.current.renderBatch(batch);

    // Re-apply search highlights after rendering new content
    if (searchQueryRef.current) {
      frameRef.current?.search(searchQueryRef.current);
    }
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
  // Re-run when outputs array ref changes so new content gets highlighted
  useEffect(() => {
    if (shouldIsolate) return; // iframe reports its own count via search_results
    if (!searchQuery || !inDomOutputRef.current || outputs.length === 0) {
      // Only report 0 if we were previously tracking matches for this cell
      if (searchQuery) onSearchMatchCount?.(0);
      return;
    }
    const cleanup = highlightTextInDom(inDomOutputRef.current, searchQuery);
    const count =
      inDomOutputRef.current.querySelectorAll(".global-find-match").length;
    onSearchMatchCount?.(count);
    return cleanup;
  }, [searchQuery, shouldIsolate, outputs, onSearchMatchCount]);

  // Empty state: render nothing (unless preloading iframe)
  if (outputs.length === 0 && !showPreloadedIframe) {
    return null;
  }

  // Hide the entire output area when only preloading (no visible outputs)
  const isPreloadOnly = showPreloadedIframe && outputs.length === 0;

  // pl-6 aligns the output left edge with the code editor indent.
  // pr-9 compensates for the right gutter (w-10) so that centered
  // content (widgets, plots) appears visually centered between the
  // code indent and the right edge. The CellContainer output wrapper
  // has no horizontal padding — it must live here because the iframe
  // ignores padding on its parent container.
  return (
    <div
      data-slot="output-area"
      className={cn("output-area pl-6 pr-9", isPreloadOnly && "hidden", className)}
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
              ? `Show ${outputs.length} output${outputs.length > 1 ? "s" : ""}`
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
                onMouseDown={onIframeMouseDown}
                onWidgetUpdate={onWidgetUpdate}
                onMessage={handleIframeMessage}
                onError={handleIframeError}
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
