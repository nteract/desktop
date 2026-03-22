/**
 * @nteract/runtimed — Transport-agnostic JavaScript bindings for
 * nteract notebook documents.
 *
 * Wraps the runtimed WASM CRDT engine with a clean API for sync,
 * cell mutations, and runtime state.
 *
 * @example
 * ```ts
 * import { SyncEngine, FrameType } from "@nteract/runtimed";
 * import type { NotebookTransport, SyncableHandle } from "@nteract/runtimed";
 *
 * const engine = new SyncEngine(handle, transport);
 * engine.on("cells_changed", (e) => console.log(e.changeset));
 * engine.on("broadcast", (e) => console.log(e.payload));
 * engine.on("runtime_state_changed", (e) => console.log(e.state));
 * engine.start();
 *
 * // After local CRDT mutations:
 * handle.update_source("cell-1", "print('hello')");
 * engine.scheduleFlush();
 *
 * // Before execution (flush immediately):
 * await engine.flush();
 * await transport.sendRequest({ action: "execute_cell", cell_id: "cell-1" });
 *
 * // Cleanup:
 * engine.stop();
 * ```
 *
 * @module
 */

// ── Transport ────────────────────────────────────────────────────────

export { FrameType } from "./transport.ts";
export type {
  FrameTypeValue,
  TypedFrame,
  NotebookTransport,
  Unsubscribe,
} from "./transport.ts";

// ── Sync Engine ──────────────────────────────────────────────────────

export { SyncEngine } from "./sync-engine.ts";
export type {
  SyncableHandle,
  HandleGetter,
  SyncEngineOptions,
  SyncEngineEvent,
  SyncEngineEventType,
} from "./sync-engine.ts";

// ── Frame Events (from WASM) ─────────────────────────────────────────

export type {
  FrameEvent,
  SyncAppliedEvent,
  BroadcastEvent,
  PresenceEvent,
  RuntimeStateSyncAppliedEvent,
  UnknownFrameEvent,
} from "./sync-engine.ts";

// ── Cell Changeset ───────────────────────────────────────────────────

export type {
  CellChangeset,
  ChangedCell,
  ChangedFields,
  TextAttribution,
} from "./sync-engine.ts";
