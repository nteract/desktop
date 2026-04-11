export { CommBridgeManager, createCommBridgeManager } from "./comm-bridge-manager";
// Message protocol types
export type {
  ClearMessage,
  EvalMessage,
  EvalResultMessage,
  IframeErrorMessage,
  // Utilities
  IframeMessage,
  // Iframe → Parent
  IframeToParentMessage,
  LinkClickMessage,
  MessageType,
  // Parent → Iframe
  ParentToIframeMessage,
  PingMessage,
  PongMessage,
  ReadyMessage,
  RenderCompleteMessage,
  RenderMessage,
  RenderPayload,
  ResizeMessage,
  ThemeMessage,
  WidgetStateMessage,
  WidgetUpdateMessage,
} from "./frame-bridge";
export { isIframeMessage, isMessageType } from "./frame-bridge";
export type { FrameHtmlOptions } from "./frame-html";
// HTML template generator
export { createFrameBlobUrl, generateFrameHtml } from "./frame-html";
// Security testing component
export { IsolationTest } from "./IsolationTest";
export type { IsolatedFrameHandle, IsolatedFrameProps } from "./isolated-frame";
export { IsolatedFrame } from "./isolated-frame";
// Provider and hook for renderer bundle
export { IsolatedRendererProvider, useIsolatedRenderer } from "./isolated-renderer-context";
