/**
 * SyncableHandle — minimal interface for the WASM NotebookHandle.
 *
 * The SyncEngine operates against this interface rather than the concrete
 * WASM class, enabling testing with mocks and future alternative
 * implementations.
 *
 * Methods mirror the subset of `NotebookHandle` used by the sync pipeline.
 * Cell mutation methods (add_cell, update_source, etc.) are NOT part of
 * this interface — they're used directly by consumers, not the engine.
 */

import type { CellChangeset } from "./cell-changeset";

// ── FrameEvent ───────────────────────────────────────────────────────

/** Attribution for text changes, produced by WASM sync. */
export interface TextAttribution {
  cell_id: string;
  index: number;
  text: string;
  deleted: number;
  actors: string[];
}

/**
 * Typed event returned by WASM `receive_frame()`.
 *
 * Event types:
 * - `sync_applied` — Automerge sync message applied successfully
 * - `broadcast` — Daemon broadcast (kernel status, output, etc.)
 * - `presence` — Remote peer presence update
 * - `runtime_state_sync_applied` — RuntimeStateDoc sync applied
 * - `sync_error` — Sync failed, doc rebuilt + sync state normalized, reply restarts negotiation
 * - `runtime_state_sync_error` — RuntimeState sync failed, same recovery pattern
 * - `unknown` — Unrecognized frame type
 */
export interface FrameEvent {
  type: string;
  changed?: boolean;
  changeset?: CellChangeset;
  attributions?: TextAttribution[];
  /** Inline sync reply bytes from receive_frame (#1067 fix).
   *  Also used for recovery replies in sync_error / runtime_state_sync_error events. */
  reply?: number[];
  payload?: unknown;
  /** RuntimeState from RuntimeStateSyncApplied. */
  state?: unknown;
  /** Cell IDs whose outputs changed in RuntimeStateDoc (from WASM-side diff). */
  output_changed_cells?: string[];
}

// ── SyncableHandle ───────────────────────────────────────────────────

export interface SyncableHandle {
  /**
   * Process an inbound frame from the daemon.
   *
   * Returns an array of typed events (sync_applied, broadcast, presence,
   * runtime_state_sync_applied, unknown).
   */
  receive_frame(bytes: Uint8Array): FrameEvent[] | null;

  /**
   * Flush local Automerge changes into a sync message.
   *
   * Returns the message bytes, or null if there are no pending changes.
   * Advances internal sync state (sent_hashes) — call `cancel_last_flush()`
   * if the send fails.
   */
  flush_local_changes(): Uint8Array | null;

  /**
   * Roll back the sync state advanced by the last `flush_local_changes()`.
   *
   * Prevents sent_hashes from permanently filtering out change data
   * the daemon never received.
   */
  cancel_last_flush(): void;

  /**
   * Flush RuntimeStateDoc sync message.
   *
   * Returns the message bytes, or null if there are no pending changes.
   */
  flush_runtime_state_sync(): Uint8Array | null;

  /** Roll back the last RuntimeStateDoc flush. */
  cancel_last_runtime_state_flush(): void;

  /** Generate a sync reply for the RuntimeStateDoc. */
  generate_runtime_state_sync_reply(): Uint8Array | null;

  /**
   * Flush PoolDoc sync message.
   *
   * Returns the message bytes, or null if there are no pending changes.
   */
  flush_pool_state_sync(): Uint8Array | null;

  /** Roll back the last PoolDoc flush. */
  cancel_last_pool_state_flush(): void;

  /** Generate a sync reply for the PoolDoc. */
  generate_pool_state_sync_reply(): Uint8Array | null;

  /** Reset sync state so the next flush requests the full document. */
  reset_sync_state(): void;

  /** Number of cells in the document. */
  cell_count(): number;

  /**
   * Resolve ContentRef values in a comm's state.
   *
   * Returns `{ state, buffer_paths }` where blob ContentRefs are replaced
   * with plain URL strings and inline ContentRefs are unwrapped.
   * `buffer_paths` records which JSON paths were resolved from blob refs.
   *
   * Returns undefined if blob_port is not set or the comm doesn't exist.
   *
   * Optional — implementations that don't support comm state resolution
   * (e.g. test mocks) may omit this method.
   */
  resolve_comm_state?(
    comm_id: string,
  ):
    | { state: Record<string, unknown>; buffer_paths: string[][] }
    | undefined;
}
