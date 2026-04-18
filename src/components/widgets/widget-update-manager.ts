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
 *
 * All that's left here is the fallback path for when the CRDT writer
 * isn't yet wired up (early bootstrap), plus binary buffer storage:
 * the CRDT doesn't carry ArrayBuffers, so we keep buffers on the local
 * widget model until outbound sync picks them up via another path.
 */

import type { WidgetStore } from "./widget-store";

type CrdtCommWriter = (commId: string, patch: Record<string, unknown>) => void;

export interface WidgetUpdateManagerOptions {
  getStore: () => WidgetStore | null;
  getCrdtWriter: () => CrdtCommWriter | null;
}

export class WidgetUpdateManager {
  private readonly getStore: () => WidgetStore | null;
  private readonly getCrdtWriter: () => CrdtCommWriter | null;

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
   * `setCrdtCommWriter` has run), falls back to a direct store
   * update so the UI isn't completely unresponsive during startup.
   *
   * Binary buffers aren't representable in the Automerge CRDT, so we
   * preserve the legacy behavior of stashing them on the local widget
   * model. Kernel delivery of binary buffers goes through the
   * SendComm RPC path in `use-comm-router.ts` (untouched by this
   * refactor).
   */
  updateAndPersist(commId: string, patch: Record<string, unknown>, buffers?: ArrayBuffer[]): void {
    const writer = this.getCrdtWriter();
    if (writer) {
      writer(commId, patch);
    } else {
      // Bootstrap fallback: CRDT writer wasn't registered yet. Mirror
      // the pre-A2 behavior so early-session widget interactions
      // (before App.tsx's setCrdtCommWriter effect has run) still
      // render. projectLocalState will pick up future writes.
      this.getStore()?.updateModel(commId, patch, buffers);
      return;
    }
    if (buffers?.length) {
      // Buffers ride alongside the CRDT state on the local widget
      // model. Kernel delivery is handled elsewhere (SendComm RPC).
      this.getStore()?.updateModel(commId, {}, buffers);
    }
  }

  /**
   * Reset any per-comm bookkeeping. Kept as a no-op for API
   * compatibility with the pre-A2 manager, which tracked optimistic
   * keys and debounce timers across kernel restarts. There is no
   * per-comm state to reset anymore.
   */
  reset(): void {}

  /**
   * Tear down. Nothing to clean up post-A2 — no pending timers, no
   * accumulated state.
   */
  dispose(): void {}

  /**
   * Drop per-comm state on comm_close. Also a no-op post-A2.
   */
  clearComm(_commId: string): void {}
}
