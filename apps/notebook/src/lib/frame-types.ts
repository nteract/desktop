/**
 * Frame type constants matching `notebook_doc::frame_types` in Rust.
 *
 * These correspond to the first byte of each typed frame payload on
 * notebook sync connections. Defined here so the frontend can reference
 * them without magic numbers.
 *
 * Outbound frames flow through `host.transport.sendFrame(frameType, payload)`
 * — there's no longer a top-level `sendFrame()` helper that reaches for
 * Tauri directly.
 */
export const frame_types = {
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
  /** PoolStateSync message (binary Automerge sync for PoolDoc, global). */
  POOL_STATE_SYNC: 0x06,
} as const;
