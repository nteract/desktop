/**
 * Frame type constants matching `notebook_doc::frame_types` in Rust.
 *
 * These correspond to the first byte of each typed frame payload on
 * notebook sync connections. Defined here so the frontend can reference
 * them without magic numbers.
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
} as const;
