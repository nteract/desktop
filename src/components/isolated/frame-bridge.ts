export interface EvalMessage {
  type: "eval";
  payload: {
    /** JavaScript code to evaluate in the iframe context */
    code: string;
  };
}

/**
 * Render output content in the iframe.
 */
export interface RenderMessage {
  type: "render";
  payload: RenderPayload;
}

export interface RenderPayload {
  /** MIME type of the content (e.g., "text/html", "text/markdown") */
  mimeType: string;
  /** The content data (format depends on MIME type) */
  data: unknown;
  /** Optional metadata for the output */
  metadata?: Record<string, unknown>;
  /** Cell ID this output belongs to (for routing) */
  cellId?: string;
  /** Output index within the cell */
  outputIndex?: number;
  /** If true, append to existing outputs instead of replacing */
  append?: boolean;
  /** If true, replace all existing outputs with this single output */
  replace?: boolean;
}

/**
 * Update widget state in the iframe.
 */
export interface WidgetStateMessage {
  type: "widget_state";
  payload: {
    /** Comm ID of the widget */
    commId: string;
    /** Updated state to merge */
    state: Record<string, unknown>;
    /** Optional buffers (base64 encoded) */
    buffers?: string[];
  };
}

/**
 * Sync theme with the iframe.
 */
export interface ThemeMessage {
  type: "theme";
  payload: {
    /** Whether dark mode is active */
    isDark: boolean;
    /** Optional CSS variables to inject */
    cssVariables?: Record<string, string>;
  };
}

/**
 * Ping the iframe (for health checks and latency measurement).
 */
export interface PingMessage {
  type: "ping";
  payload?: {
    sentAt: number;
  };
}

/**
 * Clear all rendered content in the iframe.
 */
export interface ClearMessage {
  type: "clear";
}

// --- Widget Comm Protocol: Parent → Iframe ---

/**
 * Forward a comm_open message to the iframe.
 * Sent when a widget model is created by the kernel.
 */
export interface CommOpenMessage {
  type: "comm_open";
  payload: {
    /** Comm ID of the widget */
    commId: string;
    /** Target name (e.g., "jupyter.widget") */
    targetName: string;
    /** Initial widget state */
    state: Record<string, unknown>;
    /** Buffer paths for binary data reconstruction */
    bufferPaths?: string[][];
    /** Binary buffers (transferred via structured clone) */
    buffers?: ArrayBuffer[];
  };
}

/**
 * Forward a comm_msg to the iframe.
 * Sent for state updates and custom messages from kernel.
 */
export interface CommMsgMessage {
  type: "comm_msg";
  payload: {
    /** Comm ID of the widget */
    commId: string;
    /** Message method: "update" or "custom" */
    method: "update" | "custom";
    /** State patch (for update) or custom content (for custom) */
    data: Record<string, unknown>;
    /** Buffer paths for binary data reconstruction */
    bufferPaths?: string[][];
    /** Binary buffers (transferred via structured clone) */
    buffers?: ArrayBuffer[];
  };
}

/**
 * Forward a comm_close message to the iframe.
 * Sent when a widget is destroyed by the kernel.
 */
export interface CommCloseMessage {
  type: "comm_close";
  payload: {
    /** Comm ID of the widget to close */
    commId: string;
  };
}

/**
 * Sync all existing widget models to the iframe.
 * Sent on iframe ready to bootstrap existing widgets.
 */
export interface CommSyncMessage {
  type: "comm_sync";
  payload: {
    /** Array of existing models to sync */
    models: Array<{
      commId: string;
      targetName: string;
      state: Record<string, unknown>;
      buffers?: ArrayBuffer[];
    }>;
  };
}

/**
 * Signal that the parent's comm bridge is ready.
 * Iframe should respond with widget_ready to trigger comm_sync.
 */
export interface BridgeReadyMessage {
  type: "bridge_ready";
}

// --- Global Find: Parent → Iframe ---

/**
 * Search for text within the iframe's rendered content.
 * The iframe should highlight all matches and report the count.
 */
export interface SearchMessage {
  type: "search";
  payload: {
    /** The search query string (empty string clears search) */
    query: string;
    /** Whether the search should be case-sensitive */
    caseSensitive?: boolean;
  };
}

/**
 * Navigate to a specific match in the iframe's search results.
 */
export interface SearchNavigateMessage {
  type: "search_navigate";
  payload: {
    /** The index of the match to navigate to (0-based) */
    matchIndex: number;
  };
}

/**
 * All message types that can be sent from parent to iframe.
 */
export type ParentToIframeMessage =
  | EvalMessage
  | RenderMessage
  | WidgetStateMessage
  | ThemeMessage
  | PingMessage
  | ClearMessage
  | CommOpenMessage
  | CommMsgMessage
  | CommCloseMessage
  | CommSyncMessage
  | BridgeReadyMessage
  | SearchMessage
  | SearchNavigateMessage;

// --- Message Types: Iframe → Parent ---

/**
 * Iframe has finished loading and is ready to receive messages.
 */
export interface ReadyMessage {
  type: "ready";
}

/**
 * Response to a ping message.
 */
export interface PongMessage {
  type: "pong";
  payload: {
    receivedAt: number;
    /** Echo back the payload from the ping */
    echo?: unknown;
  };
}

/**
 * Result of evaluating code in the iframe.
 */
export interface EvalResultMessage {
  type: "eval_result";
  payload: {
    success: boolean;
    result?: string;
    error?: string;
  };
}

/**
 * Iframe content has finished rendering.
 */
export interface RenderCompleteMessage {
  type: "render_complete";
  payload?: {
    /** Height of the rendered content */
    height?: number;
  };
}

/**
 * Iframe content size has changed.
 */
export interface ResizeMessage {
  type: "resize";
  payload: {
    /** New height of the content */
    height: number;
    /** New width of the content (optional) */
    width?: number;
  };
}

/**
 * User clicked a link in the iframe.
 */
export interface LinkClickMessage {
  type: "link_click";
  payload: {
    /** The URL that was clicked */
    url: string;
    /** Whether it was a ctrl/cmd click */
    newTab: boolean;
  };
}

/**
 * User double-clicked in the iframe.
 */
export interface DoubleClickMessage {
  type: "dblclick";
}

/**
 * Widget state was updated in the iframe (needs to sync to kernel).
 */
export interface WidgetUpdateMessage {
  type: "widget_update";
  payload: {
    /** Comm ID of the widget */
    commId: string;
    /** Updated state */
    state: Record<string, unknown>;
    /** Optional buffers (base64 encoded) */
    buffers?: string[];
  };
}

/**
 * An error occurred in the iframe.
 */
export interface IframeErrorMessage {
  type: "error";
  payload: {
    message: string;
    stack?: string;
  };
}

/**
 * The React renderer bundle has been loaded and initialized.
 * This is sent after the bundle is eval'd and React is mounted.
 */
export interface RendererReadyMessage {
  type: "renderer_ready";
}

// --- Widget Comm Protocol: Iframe → Parent ---

/**
 * Iframe widget system is ready to receive comm messages.
 * Parent should send comm_sync with existing models after this.
 */
export interface WidgetReadyMessage {
  type: "widget_ready";
}

/**
 * Widget initiated a state update or custom message.
 * Parent should forward to kernel and update its store.
 */
export interface WidgetCommMsgMessage {
  type: "widget_comm_msg";
  payload: {
    /** Comm ID of the widget */
    commId: string;
    /** Message method: "update" or "custom" */
    method: "update" | "custom";
    /** State patch or custom content */
    data: Record<string, unknown>;
    /** Buffer paths */
    bufferPaths?: string[][];
    /** Binary buffers */
    buffers?: ArrayBuffer[];
  };
}

/**
 * Widget initiated comm close.
 * Parent should forward to kernel and clean up.
 */
export interface WidgetCommCloseMessage {
  type: "widget_comm_close";
  payload: {
    /** Comm ID of the widget to close */
    commId: string;
  };
}

// --- Global Find: Iframe → Parent ---

/**
 * Report search results from the iframe.
 * Sent after processing a search message.
 */
export interface SearchResultsMessage {
  type: "search_results";
  payload: {
    /** Number of matches found */
    count: number;
  };
}

/**
 * All message types that can be sent from iframe to parent.
 */
export type IframeToParentMessage =
  | ReadyMessage
  | PongMessage
  | EvalResultMessage
  | RenderCompleteMessage
  | ResizeMessage
  | LinkClickMessage
  | DoubleClickMessage
  | WidgetUpdateMessage
  | IframeErrorMessage
  | RendererReadyMessage
  | WidgetReadyMessage
  | WidgetCommMsgMessage
  | WidgetCommCloseMessage
  | SearchResultsMessage;

// --- Utility Types ---

/**
 * All message types (for generic handling).
 */
export type IframeMessage = ParentToIframeMessage | IframeToParentMessage;

/**
 * Extract the message type string.
 */
export type MessageType = IframeMessage["type"];

/**
 * Type guard to check if a message is from the iframe.
 */
export function isIframeMessage(data: unknown): data is IframeToParentMessage {
  if (typeof data !== "object" || data === null) return false;
  const msg = data as { type?: unknown };
  return (
    typeof msg.type === "string" &&
    [
      "ready",
      "pong",
      "eval_result",
      "render_complete",
      "resize",
      "link_click",
      "dblclick",
      "widget_update",
      "error",
      "renderer_ready",
      "widget_ready",
      "widget_comm_msg",
      "widget_comm_close",
      "search_results",
    ].includes(msg.type)
  );
}

/**
 * Type guard for specific message types.
 */
export function isMessageType<T extends IframeMessage["type"]>(
  data: unknown,
  type: T,
): data is Extract<IframeMessage, { type: T }> {
  if (typeof data !== "object" || data === null) return false;
  return (data as { type?: unknown }).type === type;
}
