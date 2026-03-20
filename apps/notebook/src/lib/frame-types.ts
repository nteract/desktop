import { invoke } from "@tauri-apps/api/core";

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
  /** RuntimeStateSync message (binary Automerge sync for RuntimeStateDoc). */
  RUNTIME_STATE_SYNC: 0x05,
} as const;

/**
 * Send a typed frame to the daemon via the Tauri relay.
 *
 * Prepends the frame type byte to the payload and sends the resulting
 * `Uint8Array` as a raw binary IPC payload — no JSON serialization.
 * The Rust `send_frame` command accepts `tauri::ipc::Request` and
 * extracts the bytes directly from `InvokeBody::Raw`.
 */
export function sendFrame(
  frameType: number,
  payload: Uint8Array,
): Promise<void> {
  const frame = new Uint8Array(1 + payload.length);
  frame[0] = frameType;
  frame.set(payload, 1);
  return invoke("send_frame", frame);
}
