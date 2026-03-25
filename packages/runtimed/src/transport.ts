/**
 * NotebookTransport interface and frame type constants.
 *
 * The transport is the pluggable boundary between the SyncEngine and the
 * underlying connection to the daemon. Implementations:
 *   - TauriTransport (desktop app, lives in apps/notebook)
 *   - DirectTransport (testing, lives in this package)
 *   - WebSocketTransport (future web app)
 */

// ── Frame type constants ─────────────────────────────────────────────

/**
 * Frame type constants matching `notebook_doc::frame_types` in Rust.
 *
 * These correspond to the first byte of each typed frame payload on
 * notebook sync connections.
 */
export const FrameType = {
  /** Automerge sync message (binary). */
  AUTOMERGE_SYNC: 0x00,
  /** NotebookRequest (JSON). */
  REQUEST: 0x01,
  /** NotebookResponse (JSON). */
  RESPONSE: 0x02,
  /** NotebookBroadcast (JSON). */
  BROADCAST: 0x03,
  /** Presence (CBOR, see notebook_doc::presence). */
  PRESENCE: 0x04,
  /** RuntimeStateSync message (binary Automerge sync for RuntimeStateDoc). */
  RUNTIME_STATE_SYNC: 0x05,
} as const;

export type FrameTypeValue = (typeof FrameType)[keyof typeof FrameType];

// ── Transport interface ──────────────────────────────────────────────

/** Callback for receiving inbound frames from the daemon. */
export type FrameListener = (payload: number[]) => void;

/**
 * Pluggable connection layer between SyncEngine and the daemon.
 *
 * Implementations handle the mechanics of sending/receiving bytes
 * (Tauri IPC, WebSocket, in-memory for tests) while the SyncEngine
 * owns all sync logic.
 */
export interface NotebookTransport {
  /**
   * Send a typed frame to the daemon.
   *
   * Prepends the frame type byte to the payload and delivers the
   * resulting bytes via the underlying connection.
   */
  sendFrame(frameType: number, payload: Uint8Array): Promise<void>;

  /**
   * Register a callback for inbound frames from the daemon.
   *
   * Returns an unsubscribe function. The payload is the raw byte array
   * from the daemon (including frame type prefix, depending on transport).
   */
  onFrame(callback: FrameListener): () => void;

  /** Whether the transport is currently connected. */
  readonly connected: boolean;

  /** Tear down the transport (unlisten, close connections). */
  disconnect(): void;
}
