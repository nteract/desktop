/**
 * JSON-RPC 2.0 method definitions for nteract iframe communication.
 *
 * These methods extend the MCP Apps protocol with nteract-specific capabilities
 * for notebook output rendering, widget communication, and search.
 *
 * Standard MCP Apps methods (handled by the SDK):
 * - ui/notifications/host-context-changed → theme sync
 * - ui/notifications/size-changed → resize
 * - ui/open-link → link clicks
 * - ping → health check
 *
 * Nteract extension methods use the "nteract/" namespace.
 */

// ── Method Constants ────────────────────────────────────────────────

// Host → Iframe (Requests — expect a response)
export const NTERACT_EVAL = "nteract/eval" as const;
export const NTERACT_SEARCH = "nteract/search" as const;

// Host → Iframe (Notifications — fire-and-forget)
export const NTERACT_RENDER_OUTPUT = "nteract/renderOutput" as const;
export const NTERACT_RENDER_BATCH = "nteract/renderBatch" as const;
export const NTERACT_CLEAR_OUTPUTS = "nteract/clearOutputs" as const;
export const NTERACT_SEARCH_NAVIGATE = "nteract/searchNavigate" as const;
export const NTERACT_COMM_OPEN = "nteract/commOpen" as const;
export const NTERACT_COMM_MSG = "nteract/commMsg" as const;
export const NTERACT_COMM_CLOSE = "nteract/commClose" as const;
export const NTERACT_COMM_SYNC = "nteract/commSync" as const;
export const NTERACT_BRIDGE_READY = "nteract/bridgeReady" as const;
export const NTERACT_WIDGET_STATE = "nteract/widgetState" as const;

// Host → Iframe (Notifications) — additional
export const NTERACT_THEME = "nteract/theme" as const;
export const NTERACT_PING = "nteract/ping" as const;

// Iframe → Host (Notifications)
export const NTERACT_READY = "nteract/ready" as const;
export const NTERACT_RENDERER_READY = "nteract/rendererReady" as const;
export const NTERACT_RENDER_COMPLETE = "nteract/renderComplete" as const;
export const NTERACT_RESIZE = "nteract/resize" as const;
export const NTERACT_LINK_CLICK = "nteract/linkClick" as const;
export const NTERACT_DOUBLE_CLICK = "nteract/doubleClick" as const;
export const NTERACT_ERROR = "nteract/error" as const;
export const NTERACT_WIDGET_READY = "nteract/widgetReady" as const;
export const NTERACT_WIDGET_COMM_MSG = "nteract/widgetCommMsg" as const;
export const NTERACT_WIDGET_COMM_CLOSE = "nteract/widgetCommClose" as const;
export const NTERACT_WIDGET_UPDATE = "nteract/widgetUpdate" as const;
export const NTERACT_EVAL_RESULT = "nteract/evalResult" as const;
export const NTERACT_PONG = "nteract/pong" as const;
export const NTERACT_SEARCH_RESULTS = "nteract/searchResults" as const;
export const NTERACT_MOUSE_DOWN = "nteract/mouseDown" as const;

// ── Host → Iframe: Request Params & Results ─────────────────────────

export interface NteractEvalParams {
  code: string;
}

export interface NteractEvalResult {
  success: boolean;
  result?: string;
  error?: string;
}

export interface NteractSearchParams {
  query: string;
  caseSensitive?: boolean;
}

export interface NteractSearchResult {
  count: number;
}

// ── Host → Iframe: Notification Params ──────────────────────────────

export interface NteractRenderOutputParams {
  mimeType: string;
  data: unknown;
  metadata?: Record<string, unknown>;
  cellId?: string;
  outputIndex?: number;
  append?: boolean;
  replace?: boolean;
}

export interface NteractRenderBatchParams {
  outputs: NteractRenderOutputParams[];
}

export interface NteractSearchNavigateParams {
  matchIndex: number;
}

export interface NteractCommOpenParams {
  commId: string;
  targetName: string;
  state: Record<string, unknown>;
  bufferPaths?: string[][];
  buffers?: ArrayBuffer[];
}

export interface NteractCommMsgParams {
  commId: string;
  method: "update" | "custom";
  data: Record<string, unknown>;
  bufferPaths?: string[][];
  buffers?: ArrayBuffer[];
}

export interface NteractCommCloseParams {
  commId: string;
}

export interface NteractCommSyncParams {
  models: Array<{
    commId: string;
    targetName: string;
    state: Record<string, unknown>;
    buffers?: ArrayBuffer[];
  }>;
}

export interface NteractWidgetStateParams {
  commId: string;
  state: Record<string, unknown>;
  buffers?: string[];
}

// ── Iframe → Host: Notification Params ──────────────────────────────

export interface NteractRenderCompleteParams {
  height?: number;
}

export interface NteractWidgetCommMsgParams {
  commId: string;
  method: "update" | "custom";
  data: Record<string, unknown>;
  bufferPaths?: string[][];
  buffers?: ArrayBuffer[];
}

export interface NteractWidgetCommCloseParams {
  commId: string;
}

export interface NteractWidgetUpdateParams {
  commId: string;
  state: Record<string, unknown>;
  buffers?: string[];
}

