/**
 * Widget Update Manager — CRDT-first widget state persistence.
 *
 * Historical note (was #1580): this used to be a dual-write path —
 * synchronous optimistic `store.updateModel` for instant UI feedback,
 * plus debounced CRDT writes with echo-suppression bookkeeping to
 * prevent in-flight drags from being clobbered by stale daemon echoes.
 * That reconciliation grew the widget-sync stall described in
 * `docs/superpowers/specs/2026-04-17-widget-sync-stall-design.md`.
 *
 * Track A collapses it into a single source of truth: every write goes
 * to the CRDT, and the `WidgetStore` updates purely from
 * `commChanges$` — local writes included, because `SyncEngine.projectLocalState`
 * fires the projection synchronously after `set_comm_state_batch`.
 */

import type { WidgetStore } from "./widget-store";

type CrdtCommWriter = (commId: string, patch: Record<string, unknown>) => void;

export interface WidgetUpdateManagerOptions {
  getStore: () => WidgetStore | null;
  getCrdtWriter: () => CrdtCommWriter | null;
}

/**
 * Poll interval for draining queued writes while the CRDT writer is
 * still null. Keeps the drain small & bounded without needing the
 * caller to explicitly kick the manager.
 */
const DRAIN_POLL_MS = 50;

export class WidgetUpdateManager {
  private readonly getStore: () => WidgetStore | null;
  private readonly getCrdtWriter: () => CrdtCommWriter | null;

  /**
   * Patches that arrived before the CRDT writer was registered.
   * Keyed by commId, last-wins on key collision (each patch is
   * merged onto the previous with `{ ...existing, ...patch }`).
   * Drained as soon as a writer becomes available.
   */
  private pendingQueue = new Map<string, Record<string, unknown>>();
  private drainTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(opts: WidgetUpdateManagerOptions) {
    this.getStore = opts.getStore;
    this.getCrdtWriter = opts.getCrdtWriter;
  }

  /**
   * Persist a widget state update.
   *
   * Writes the patch to the CRDT via the injected writer (which also
   * fires `projectLocalState` on the sync engine, so the widget store
   * sees the update in the same tick). No optimistic store write, no
   * echo suppression — the CRDT is the single source of truth.
   *
   * If the CRDT writer isn't available yet (early bootstrap before
   * `setCrdtCommWriter` has run), the patch is queued and the UI
   * still gets an instant store mirror so the user isn't stuck on a
   * blank control. When the writer shows up, queued patches flush in
   * insertion order and the daemon receives them as if the user had
   * acted slightly later.
   *
   * Binary buffers aren't representable in the Automerge CRDT, so we
   * mirror them into the local widget model. Kernel delivery of
   * binary buffers goes through the SendComm RPC path in
   * `use-comm-router.ts` (untouched by this refactor).
   */
  updateAndPersist(commId: string, patch: Record<string, unknown>, buffers?: ArrayBuffer[]): void {
    const writer = this.getCrdtWriter();
    if (writer) {
      // Drain anything queued before this writer became available.
      this.drainPending(writer);
      writer(commId, patch);
    } else {
      // Bootstrap: accumulate on the pending queue until the CRDT
      // writer shows up. We also mirror the patch to the local store
      // so the UI doesn't stall — `projectLocalState` will re-emit
      // the value once the writer drains our queue.
      const existing = this.pendingQueue.get(commId) ?? {};
      this.pendingQueue.set(commId, { ...existing, ...patch });
      this.getStore()?.updateModel(commId, patch, buffers);
      this.scheduleDrain();
    }
    if (writer && buffers?.length) {
      // Buffers ride alongside the CRDT state on the local widget
      // model. Kernel delivery is handled elsewhere (SendComm RPC).
      this.getStore()?.updateModel(commId, {}, buffers);
    }
  }

  /**
   * Drain the pending queue through the supplied writer. Called
   * opportunistically on the next `updateAndPersist` once the writer
   * is registered, and on the polling timer for the "no new writes
   * arrived but writer just became available" case.
   */
  private drainPending(writer: CrdtCommWriter): void {
    if (this.pendingQueue.size === 0) return;
    for (const [commId, patch] of this.pendingQueue) {
      writer(commId, patch);
    }
    this.pendingQueue.clear();
    if (this.drainTimer) {
      clearTimeout(this.drainTimer);
      this.drainTimer = null;
    }
  }

  private scheduleDrain(): void {
    if (this.drainTimer) return;
    this.drainTimer = setTimeout(() => {
      this.drainTimer = null;
      const writer = this.getCrdtWriter();
      if (writer) {
        this.drainPending(writer);
      } else if (this.pendingQueue.size > 0) {
        this.scheduleDrain();
      }
    }, DRAIN_POLL_MS);
  }

  /**
   * Reset any per-comm bookkeeping. Also drops the pending queue:
   * a kernel restart invalidates optimistic state the old kernel was
   * going to receive, so replaying queued writes into a fresh kernel
   * would be worse than losing them.
   */
  reset(): void {
    this.pendingQueue.clear();
    if (this.drainTimer) {
      clearTimeout(this.drainTimer);
      this.drainTimer = null;
    }
  }

  /**
   * Tear down.
   */
  dispose(): void {
    this.reset();
  }

  /**
   * Drop pending state for a specific comm (called on comm_close).
   */
  clearComm(commId: string): void {
    this.pendingQueue.delete(commId);
  }
}
