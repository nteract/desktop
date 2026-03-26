/**
 * SyncEngine unit tests using mock handles.
 *
 * Proves the engine's lifecycle, coalescing, rollback, retry, and
 * observable emission without requiring WASM or a real daemon.
 *
 * Time-dependent tests use RxJS VirtualTimeScheduler instead of vi.useFakeTimers.
 */

import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { VirtualTimeScheduler, VirtualAction } from "rxjs";
import { SyncEngine } from "../src/sync-engine";
import { DirectTransport } from "../src/direct-transport";
import { FrameType } from "../src/transport";
import { mergeChangesets } from "../src/cell-changeset";
import { diffExecutions } from "../src/runtime-state";
import type { SyncableHandle, FrameEvent } from "../src/handle";
import type { CellChangeset } from "../src/cell-changeset";
import type { RuntimeState } from "../src/runtime-state";

// ── Mock factories ──────────────────────────────────────────────────

function createMockHandle(overrides: Partial<SyncableHandle> = {}): SyncableHandle {
  return {
    receive_frame: vi.fn(() => []),
    flush_local_changes: vi.fn(() => null),
    cancel_last_flush: vi.fn(),
    flush_runtime_state_sync: vi.fn(() => null),
    cancel_last_runtime_state_flush: vi.fn(),
    generate_runtime_state_sync_reply: vi.fn(() => null),
    reset_sync_state: vi.fn(),
    cell_count: vi.fn(() => 0),
    ...overrides,
  };
}

function createMockServerHandle() {
  return {
    flush_local_changes: vi.fn(() => null),
    receive_sync_message: vi.fn(() => true),
    reset_sync_state: vi.fn(),
  };
}

function syncAppliedEvent(opts: {
  changed?: boolean;
  changeset?: CellChangeset;
  reply?: number[];
  attributions?: FrameEvent["attributions"];
} = {}): FrameEvent {
  return {
    type: "sync_applied",
    changed: opts.changed ?? false,
    changeset: opts.changeset,
    reply: opts.reply,
    attributions: opts.attributions,
  };
}

function broadcastEvent(payload: unknown): FrameEvent {
  return { type: "broadcast", payload };
}

function presenceEvent(payload: unknown): FrameEvent {
  return { type: "presence", payload };
}

function runtimeStateSyncEvent(state: RuntimeState): FrameEvent {
  return { type: "runtime_state_sync_applied", changed: true, state };
}

function makeRuntimeState(
  executions: Record<
    string,
    { cell_id: string; status: string; execution_count: number | null; success: boolean | null }
  >,
): RuntimeState {
  return {
    kernel: {
      status: "idle",
      starting_phase: "",
      name: "python3",
      language: "python",
      env_source: "",
    },
    queue: { executing: null, queued: [] },
    env: {
      in_sync: true,
      added: [],
      removed: [],
      channels_changed: false,
      deno_changed: false,
    },
    trust: { status: "trusted", needs_approval: false },
    last_saved: null,
    executions: executions as RuntimeState["executions"],
  };
}

const EMPTY_CHANGESET: CellChangeset = {
  changed: [],
  added: [],
  removed: [],
  order_changed: false,
};

// ── Helper: advance scheduler to a given time ───────────────────────

/**
 * Advance the virtual clock by `ms` milliseconds.
 *
 * Sets `maxFrames` so `flush()` stops at the target time instead of
 * spinning forever on repeating operators like `bufferTime`.
 */
function advanceBy(scheduler: VirtualTimeScheduler, ms: number): void {
  const target = scheduler.frame + ms;
  scheduler.maxFrames = target;
  scheduler.schedule(() => {}, ms);
  scheduler.flush();
}

// ── Tests ────────────────────────────────────────────────────────────

describe("SyncEngine", () => {
  let handle: SyncableHandle;
  let server: ReturnType<typeof createMockServerHandle>;
  let transport: DirectTransport;
  let scheduler: VirtualTimeScheduler;

  beforeEach(() => {
    handle = createMockHandle();
    server = createMockServerHandle();
    transport = new DirectTransport(server);
    scheduler = new VirtualTimeScheduler(VirtualAction, Infinity);
  });

  /** Helper: create engine with the VirtualTimeScheduler injected */
  function createEngine(opts?: { getHandle?: () => SyncableHandle | null }): SyncEngine {
    return new SyncEngine({
      getHandle: opts?.getHandle ?? (() => handle),
      transport,
      scheduler,
    });
  }

  // ── Lifecycle ──────────────────────────────────────────────────

  describe("lifecycle", () => {
    it("starts and stops cleanly", () => {
      const engine = createEngine();
      expect(engine.running).toBe(false);
      engine.start();
      expect(engine.running).toBe(true);
      engine.stop();
      expect(engine.running).toBe(false);
    });

    it("start is idempotent", () => {
      const engine = createEngine();
      engine.start();
      engine.start(); // should not throw or double-subscribe
      expect(engine.running).toBe(true);
      engine.stop();
    });

    it("stop is idempotent", () => {
      const engine = createEngine();
      engine.start();
      engine.stop();
      engine.stop(); // should not throw
      expect(engine.running).toBe(false);
    });
  });

  // ── Initial sync ──────────────────────────────────────────────

  describe("initial sync", () => {
    it("emits initialSyncComplete$ when changed:true arrives", () => {
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        syncAppliedEvent({ changed: true }),
      ]);

      const engine = createEngine();
      engine.start();

      let completed = false;
      engine.initialSyncComplete$.subscribe(() => {
        completed = true;
      });

      transport.deliver(Array.from([0x00, 1, 2, 3]));
      expect(completed).toBe(true);
      engine.stop();
    });

    it("does not emit initialSyncComplete$ on changed:false", () => {
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        syncAppliedEvent({ changed: false }),
      ]);

      const engine = createEngine();
      engine.start();

      let completed = false;
      engine.initialSyncComplete$.subscribe(() => {
        completed = true;
      });

      transport.deliver(Array.from([0x00, 1, 2, 3]));
      advanceBy(scheduler, 100);

      expect(completed).toBe(false);
      engine.stop();
    });

    it("retries sync after timeout when initial sync stalls", () => {
      // First frame: changed:false (handshake, no content)
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        syncAppliedEvent({ changed: false }),
      ]);

      // flush_local_changes returns a sync message for the retry
      (handle.flush_local_changes as ReturnType<typeof vi.fn>).mockReturnValue(
        new Uint8Array([1, 2, 3]),
      );

      const engine = createEngine();
      engine.start();
      transport.deliver(Array.from([0x00, 1, 2, 3]));

      // Advance past the 3s retry timeout
      advanceBy(scheduler, 3100);

      // Engine should have called reset_sync_state + flush for retry
      expect(handle.reset_sync_state).toHaveBeenCalled();
      engine.stop();
    });

    it("handshake round restarts the retry timer via switchMap", () => {
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        syncAppliedEvent({ changed: false }),
      ]);
      (handle.flush_local_changes as ReturnType<typeof vi.fn>).mockReturnValue(
        new Uint8Array([1, 2, 3]),
      );

      const engine = createEngine();
      engine.start();

      // First handshake round at t=0 — arms the 3s retry timer
      transport.deliver(Array.from([0x00, 1]));

      // Advance 2.5s — timer not yet fired
      advanceBy(scheduler, 2500);
      expect(handle.reset_sync_state).not.toHaveBeenCalled();

      // Another handshake round restarts the 3s timer (switchMap)
      transport.deliver(Array.from([0x00, 2]));

      // Advance 2.5s more (5s total, but only 2.5s since last round)
      advanceBy(scheduler, 2500);
      expect(handle.reset_sync_state).not.toHaveBeenCalled();

      // Advance 1s more (3.5s since last round — past 3s timeout)
      advanceBy(scheduler, 1000);
      expect(handle.reset_sync_state).toHaveBeenCalled();
      engine.stop();
    });
  });

  // ── Broadcasts ────────────────────────────────────────────────

  describe("broadcasts", () => {
    it("emits broadcast payloads on broadcasts$", () => {
      const broadcastPayload = { event: "kernel_status", status: "busy" };
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        broadcastEvent(broadcastPayload),
      ]);

      const engine = createEngine();
      engine.start();

      const received: unknown[] = [];
      engine.broadcasts$.subscribe((p) => received.push(p));

      transport.deliver(Array.from([0x03, 1]));
      expect(received).toHaveLength(1);
      expect(received[0]).toEqual(broadcastPayload);
      engine.stop();
    });

    it("emits text_attribution as broadcast", () => {
      const attributions = [
        { cell_id: "c1", index: 0, text: "hi", deleted: 0, actors: ["a"] },
      ];
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        syncAppliedEvent({ changed: true, attributions }),
      ]);

      const engine = createEngine();
      engine.start();

      const received: unknown[] = [];
      engine.broadcasts$.subscribe((p) => received.push(p));

      transport.deliver(Array.from([0x00, 1]));
      expect(received).toHaveLength(1);
      expect(received[0]).toEqual({
        type: "text_attribution",
        attributions,
      });
      engine.stop();
    });
  });

  // ── Presence ──────────────────────────────────────────────────

  describe("presence", () => {
    it("emits presence payloads on presence$", () => {
      const presencePayload = { type: "update", peer: "alice", cursor: {} };
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        presenceEvent(presencePayload),
      ]);

      const engine = createEngine();
      engine.start();

      const received: unknown[] = [];
      engine.presence$.subscribe((p) => received.push(p));

      transport.deliver(Array.from([0x04, 1]));
      expect(received).toHaveLength(1);
      expect(received[0]).toEqual(presencePayload);
      engine.stop();
    });
  });

  // ── Cell changes (coalescing) ─────────────────────────────────

  describe("cellChanges$", () => {
    it("emits coalesced changesets after initial sync", () => {
      let callCount = 0;
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockImplementation(() => {
        callCount++;
        if (callCount === 1) {
          // First call: initial sync
          return [syncAppliedEvent({ changed: true })];
        }
        // Subsequent calls: steady-state changes
        return [
          syncAppliedEvent({
            changed: true,
            changeset: {
              changed: [{ cell_id: "c1", fields: { source: true } }],
              added: [],
              removed: [],
              order_changed: false,
            },
          }),
        ];
      });

      const engine = createEngine();
      engine.start();

      // Complete initial sync
      transport.deliver(Array.from([0x00, 1]));

      // Subscribe to cell changes
      const emissions: (CellChangeset | null)[] = [];
      engine.cellChanges$.subscribe((cs) => emissions.push(cs));

      // Send steady-state frame
      transport.deliver(Array.from([0x00, 2]));

      // Advance past coalescing window (32ms)
      advanceBy(scheduler, 50);

      expect(emissions).toHaveLength(1);
      const changeset = emissions[0];
      expect(changeset).not.toBeNull();
      expect(changeset!.changed[0].cell_id).toBe("c1");
      expect(changeset!.changed[0].fields.source).toBe(true);
      engine.stop();
    });

    it("emits null changeset when WASM has no changeset", () => {
      let callCount = 0;
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockImplementation(() => {
        callCount++;
        if (callCount === 1) {
          return [syncAppliedEvent({ changed: true })];
        }
        return [syncAppliedEvent({ changed: true })]; // no changeset
      });

      const engine = createEngine();
      engine.start();

      // Complete initial sync
      transport.deliver(Array.from([0x00, 1]));

      const emissions: (CellChangeset | null)[] = [];
      engine.cellChanges$.subscribe((cs) => emissions.push(cs));

      transport.deliver(Array.from([0x00, 2]));
      advanceBy(scheduler, 50);

      expect(emissions).toHaveLength(1);
      expect(emissions[0]).toBeNull();
      engine.stop();
    });

    it("merges multiple frames within the 32ms coalescing window", () => {
      let callCount = 0;
      const changesets: CellChangeset[] = [
        { changed: [{ cell_id: "c1", fields: { source: true } }], added: [], removed: [], order_changed: false },
        { changed: [{ cell_id: "c2", fields: { outputs: true } }], added: [], removed: [], order_changed: false },
        { changed: [{ cell_id: "c1", fields: { metadata: true } }], added: [], removed: [], order_changed: false },
      ];

      (handle.receive_frame as ReturnType<typeof vi.fn>).mockImplementation(() => {
        callCount++;
        if (callCount === 1) {
          return [syncAppliedEvent({ changed: true })]; // initial sync
        }
        // Return different changesets for each subsequent frame
        return [syncAppliedEvent({ changed: true, changeset: changesets[callCount - 2] })];
      });

      const engine = createEngine();
      engine.start();

      // Complete initial sync
      transport.deliver(Array.from([0x00, 1]));

      const emissions: (CellChangeset | null)[] = [];
      engine.cellChanges$.subscribe((cs) => emissions.push(cs));

      // Send 3 frames within the 32ms window
      transport.deliver(Array.from([0x00, 2]));
      advanceBy(scheduler, 10);
      transport.deliver(Array.from([0x00, 3]));
      advanceBy(scheduler, 10);
      transport.deliver(Array.from([0x00, 4]));

      // Advance past coalescing window
      advanceBy(scheduler, 50);

      // Should get a single merged emission
      expect(emissions).toHaveLength(1);
      const cs = emissions[0]!;
      expect(cs).not.toBeNull();

      // c1 should have source + metadata merged, c2 should have outputs
      const c1 = cs.changed.find((c) => c.cell_id === "c1");
      const c2 = cs.changed.find((c) => c.cell_id === "c2");
      expect(c1).toBeDefined();
      expect(c1!.fields.source).toBe(true);
      expect(c1!.fields.metadata).toBe(true);
      expect(c2).toBeDefined();
      expect(c2!.fields.outputs).toBe(true);
      engine.stop();
    });

    it("emits separately for frames in different coalescing windows", () => {
      let callCount = 0;
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockImplementation(() => {
        callCount++;
        if (callCount === 1) {
          return [syncAppliedEvent({ changed: true })]; // initial sync
        }
        return [
          syncAppliedEvent({
            changed: true,
            changeset: {
              changed: [{ cell_id: `c${callCount}`, fields: { source: true } }],
              added: [],
              removed: [],
              order_changed: false,
            },
          }),
        ];
      });

      const engine = createEngine();
      engine.start();
      transport.deliver(Array.from([0x00, 1])); // initial sync

      const emissions: (CellChangeset | null)[] = [];
      engine.cellChanges$.subscribe((cs) => emissions.push(cs));

      // First frame + flush its coalescing window
      transport.deliver(Array.from([0x00, 2]));
      advanceBy(scheduler, 50);
      expect(emissions).toHaveLength(1);

      // Second frame in a new coalescing window
      transport.deliver(Array.from([0x00, 3]));
      advanceBy(scheduler, 50);
      expect(emissions).toHaveLength(2);

      engine.stop();
    });

    it("mixed null and valid changeset in same window forces full materialization", () => {
      let callCount = 0;
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockImplementation(() => {
        callCount++;
        if (callCount === 1) {
          return [syncAppliedEvent({ changed: true })]; // initial sync
        }
        if (callCount === 2) {
          // Valid changeset
          return [
            syncAppliedEvent({
              changed: true,
              changeset: {
                changed: [{ cell_id: "c1", fields: { source: true } }],
                added: [],
                removed: [],
                order_changed: false,
              },
            }),
          ];
        }
        // No changeset (null) — forces full materialization
        return [syncAppliedEvent({ changed: true })];
      });

      const engine = createEngine();
      engine.start();
      transport.deliver(Array.from([0x00, 1])); // initial sync

      const emissions: (CellChangeset | null)[] = [];
      engine.cellChanges$.subscribe((cs) => emissions.push(cs));

      // Send valid changeset and null changeset within same window
      transport.deliver(Array.from([0x00, 2]));
      transport.deliver(Array.from([0x00, 3]));
      advanceBy(scheduler, 50);

      // Should emit null (full materialization needed)
      expect(emissions).toHaveLength(1);
      expect(emissions[0]).toBeNull();
      engine.stop();
    });
  });

  // ── Runtime state ─────────────────────────────────────────────

  describe("runtimeState$", () => {
    it("emits runtime state on state sync", () => {
      const state: RuntimeState = {
        kernel: {
          status: "busy",
          starting_phase: "",
          name: "python3",
          language: "python",
          env_source: "",
        },
        queue: { executing: null, queued: [] },
        env: {
          in_sync: true,
          added: [],
          removed: [],
          channels_changed: false,
          deno_changed: false,
        },
        trust: { status: "trusted", needs_approval: false },
        last_saved: null,
        executions: {},
      };

      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        runtimeStateSyncEvent(state),
      ]);

      const engine = createEngine();
      engine.start();

      const received: RuntimeState[] = [];
      engine.runtimeState$.subscribe((s) => received.push(s));

      transport.deliver(Array.from([0x05, 1]));

      expect(received).toHaveLength(1);
      expect(received[0].kernel.status).toBe("busy");
      expect(received[0].kernel.name).toBe("python3");
      engine.stop();
    });
  });

  // ── Execution transitions ─────────────────────────────────────

  describe("executionTransitions$", () => {
    it("detects started transition", () => {
      const state: RuntimeState = {
        kernel: {
          status: "busy",
          starting_phase: "",
          name: "python3",
          language: "python",
          env_source: "",
        },
        queue: { executing: null, queued: [] },
        env: {
          in_sync: true,
          added: [],
          removed: [],
          channels_changed: false,
          deno_changed: false,
        },
        trust: { status: "trusted", needs_approval: false },
        last_saved: null,
        executions: {
          "exec-1": {
            cell_id: "c1",
            status: "running",
            execution_count: 1,
            success: null,
          },
        },
      };

      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        runtimeStateSyncEvent(state),
      ]);

      const engine = createEngine();
      engine.start();

      const received: import("../src/runtime-state").ExecutionTransition[][] = [];
      engine.executionTransitions$.subscribe((t) => received.push(t));

      transport.deliver(Array.from([0x05, 1]));

      expect(received).toHaveLength(1);
      expect(received[0]).toHaveLength(1);
      expect(received[0][0].kind).toBe("started");
      expect(received[0][0].cell_id).toBe("c1");
      expect(received[0][0].execution_id).toBe("exec-1");
      engine.stop();
    });
  });

  // ── Inline sync reply ─────────────────────────────────────────

  describe("sync replies", () => {
    it("sends inline sync reply via transport", () => {
      const reply = [10, 20, 30];
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        syncAppliedEvent({ changed: true, reply }),
      ]);

      const engine = createEngine();
      engine.start();
      transport.deliver(Array.from([0x00, 1]));

      // Check that a sync frame was sent
      const syncFrames = transport.sentFrames.filter(
        (f) => f.frameType === FrameType.AUTOMERGE_SYNC,
      );
      expect(syncFrames.length).toBeGreaterThanOrEqual(1);
      engine.stop();
    });

    it("rolls back sync state on send failure", async () => {
      const reply = [10, 20, 30];
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        syncAppliedEvent({ changed: true, reply }),
      ]);

      transport.simulateFailure = true;
      const engine = createEngine();
      engine.start();
      transport.deliver(Array.from([0x00, 1]));

      // Let the promise rejection propagate
      await Promise.resolve();

      expect(handle.cancel_last_flush).toHaveBeenCalled();
      engine.stop();
    });
  });

  // ── Outbound flush ────────────────────────────────────────────

  describe("flush", () => {
    it("flush() sends local changes via transport", () => {
      const syncMsg = new Uint8Array([1, 2, 3]);
      (handle.flush_local_changes as ReturnType<typeof vi.fn>).mockReturnValue(syncMsg);

      const engine = createEngine();
      engine.start();
      engine.flush();

      const syncFrames = transport.sentFrames.filter(
        (f) => f.frameType === FrameType.AUTOMERGE_SYNC,
      );
      expect(syncFrames).toHaveLength(1);
      expect(syncFrames[0].payload).toEqual(syncMsg);
      engine.stop();
    });

    it("flush() also sends RuntimeStateDoc sync", () => {
      const stateMsg = new Uint8Array([4, 5, 6]);
      (handle.flush_runtime_state_sync as ReturnType<typeof vi.fn>).mockReturnValue(stateMsg);

      const engine = createEngine();
      engine.start();
      engine.flush();

      const stateFrames = transport.sentFrames.filter(
        (f) => f.frameType === FrameType.RUNTIME_STATE_SYNC,
      );
      expect(stateFrames).toHaveLength(1);
      expect(stateFrames[0].payload).toEqual(stateMsg);
      engine.stop();
    });

    it("flush() rolls back on transport failure", async () => {
      const syncMsg = new Uint8Array([1, 2, 3]);
      (handle.flush_local_changes as ReturnType<typeof vi.fn>).mockReturnValue(syncMsg);

      transport.simulateFailure = true;
      const engine = createEngine();
      engine.start();
      engine.flush();

      await Promise.resolve();
      expect(handle.cancel_last_flush).toHaveBeenCalled();
      engine.stop();
    });

    it("scheduleFlush() debounces at 20ms", () => {
      const syncMsg = new Uint8Array([1]);
      (handle.flush_local_changes as ReturnType<typeof vi.fn>).mockReturnValue(syncMsg);

      const engine = createEngine();
      engine.start();
      engine.scheduleFlush();
      engine.scheduleFlush();
      engine.scheduleFlush();

      // No flush yet
      expect(transport.sentFrames).toHaveLength(0);

      // Advance past debounce (20ms)
      advanceBy(scheduler, 25);

      // Should have flushed exactly once
      const syncFrames = transport.sentFrames.filter(
        (f) => f.frameType === FrameType.AUTOMERGE_SYNC,
      );
      expect(syncFrames).toHaveLength(1);
      engine.stop();
    });

    it("scheduleFlush() resets debounce timer on each call", () => {
      const syncMsg = new Uint8Array([1]);
      (handle.flush_local_changes as ReturnType<typeof vi.fn>).mockReturnValue(syncMsg);

      const engine = createEngine();
      engine.start();

      // First call at t=0
      engine.scheduleFlush();

      // Advance 15ms (not yet past 20ms debounce)
      advanceBy(scheduler, 15);
      expect(transport.sentFrames).toHaveLength(0);

      // Second call resets the timer at t=15
      engine.scheduleFlush();

      // Advance 15ms more (t=30, but only 15ms since last call)
      advanceBy(scheduler, 15);
      expect(transport.sentFrames).toHaveLength(0);

      // Advance 10ms more (t=40, 25ms since last call — past 20ms debounce)
      advanceBy(scheduler, 10);

      const syncFrames = transport.sentFrames.filter(
        (f) => f.frameType === FrameType.AUTOMERGE_SYNC,
      );
      expect(syncFrames).toHaveLength(1);
      engine.stop();
    });
  });

  // ── resetAndResync ────────────────────────────────────────────

  describe("resetAndResync", () => {
    it("resets sync state and flushes", () => {
      const syncMsg = new Uint8Array([7, 8, 9]);
      (handle.flush_local_changes as ReturnType<typeof vi.fn>).mockReturnValue(syncMsg);

      const engine = createEngine();
      engine.start();
      engine.resetAndResync();

      expect(handle.reset_sync_state).toHaveBeenCalled();
      expect(transport.sentFrames.length).toBeGreaterThanOrEqual(1);
      engine.stop();
    });
  });

  // ── resetForBootstrap ─────────────────────────────────────────

  describe("resetForBootstrap", () => {
    it("emits initialSyncComplete$ again after resetForBootstrap + changed:true", () => {
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        syncAppliedEvent({ changed: true }),
      ]);

      const engine = createEngine();
      engine.start();

      // Track all emissions
      let emitCount = 0;
      engine.initialSyncComplete$.subscribe(() => {
        emitCount++;
      });

      // Complete first initial sync
      transport.deliver(Array.from([0x00, 1]));
      expect(emitCount).toBe(1);

      // Simulate daemon:ready — reset for a new bootstrap cycle
      engine.resetForBootstrap();

      // Second initial sync should emit again
      transport.deliver(Array.from([0x00, 2]));
      expect(emitCount).toBe(2);
      engine.stop();
    });

    it("does not emit cellChanges$ during initial sync phase", () => {
      let callCount = 0;
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockImplementation(() => {
        callCount++;
        return [
          syncAppliedEvent({
            changed: true,
            changeset: {
              changed: [{ cell_id: "c1", fields: { source: true } }],
              added: [],
              removed: [],
              order_changed: false,
            },
          }),
        ];
      });

      const engine = createEngine();
      engine.start();

      let cellChangeCount = 0;
      engine.cellChanges$.subscribe(() => {
        cellChangeCount++;
      });

      // First frame completes initial sync — should NOT go to cellChanges$
      transport.deliver(Array.from([0x00, 1]));
      advanceBy(scheduler, 50);
      expect(cellChangeCount).toBe(0);

      // After initial sync, steady-state frames go to cellChanges$
      transport.deliver(Array.from([0x00, 2]));
      advanceBy(scheduler, 50);
      expect(cellChangeCount).toBe(1);

      // Reset for bootstrap — back to initial sync phase
      engine.resetForBootstrap();

      // This frame should NOT go to cellChanges$ (awaiting initial sync)
      transport.deliver(Array.from([0x00, 3]));
      advanceBy(scheduler, 50);
      expect(cellChangeCount).toBe(1); // unchanged
      engine.stop();
    });
  });

  // ── Null handle safety ────────────────────────────────────────

  describe("null handle", () => {
    it("does not crash when handle is null", () => {
      const nullEngine = new SyncEngine({
        getHandle: () => null,
        transport,
        scheduler,
      });

      nullEngine.start();
      transport.deliver(Array.from([0x00, 1, 2, 3]));
      nullEngine.flush();
      nullEngine.scheduleFlush();
      nullEngine.resetAndResync();
      nullEngine.stop();
    });

    it("handle becomes null mid-pipeline after initial sync", () => {
      let returnHandle: SyncableHandle | null = handle;

      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        syncAppliedEvent({ changed: true }),
      ]);

      const engine = new SyncEngine({
        getHandle: () => returnHandle,
        transport,
        scheduler,
      });
      engine.start();

      // Complete initial sync with valid handle
      transport.deliver(Array.from([0x00, 1]));

      // Now null out the handle
      returnHandle = null;

      // Deliver frame — should not crash
      transport.deliver(Array.from([0x00, 2]));
      advanceBy(scheduler, 50);

      // Flush — should not crash or send frames
      transport.clearSentFrames();
      engine.flush();
      expect(transport.sentFrames).toHaveLength(0);
      engine.stop();
    });
  });

  // ── Multicast (frameEvents$ share) ─────────────────────────────

  describe("frameEvents$ multicast", () => {
    it("delivers events to multiple subscribers via shared observable", () => {
      const broadcastPayload = { event: "kernel_status", status: "idle" };
      const presencePayload = { type: "update", peer: "alice" };

      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        broadcastEvent(broadcastPayload),
        presenceEvent(presencePayload),
      ]);

      const engine = createEngine();
      engine.start();

      const broadcasts: unknown[] = [];
      const presences: unknown[] = [];
      engine.broadcasts$.subscribe((p) => broadcasts.push(p));
      engine.presence$.subscribe((p) => presences.push(p));

      // Single frame produces both event types
      transport.deliver(Array.from([0x03, 1]));

      expect(broadcasts).toHaveLength(1);
      expect(broadcasts[0]).toEqual(broadcastPayload);
      expect(presences).toHaveLength(1);
      expect(presences[0]).toEqual(presencePayload);
      engine.stop();
    });
  });

  // ── Edge cases ─────────────────────────────────────────────────

  describe("edge cases", () => {
    it("frame delivered after stop() does not crash or emit", () => {
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        broadcastEvent({ event: "test" }),
      ]);

      const engine = createEngine();
      engine.start();

      const received: unknown[] = [];
      engine.broadcasts$.subscribe((p) => received.push(p));

      // Stop and then deliver — should not crash
      engine.stop();
      transport.deliver(Array.from([0x03, 1]));

      expect(received).toHaveLength(0);
    });
  });

  // ── Execution lifecycle changesets ─────────────────────────────

  describe("execution lifecycle changesets", () => {
    // Registry to map frame bytes → FrameEvents for runtime state frames
    let runtimeStateFrameRegistry: Map<string, FrameEvent[]>;
    let runtimeStateFrameCounter: number;

    beforeEach(() => {
      runtimeStateFrameRegistry = new Map();
      runtimeStateFrameCounter = 0;
    });

    function deliverRuntimeState(state: RuntimeState): void {
      runtimeStateFrameCounter++;
      const frameBytes = [0x05, runtimeStateFrameCounter];
      const key = Array.from(frameBytes).join(",");
      runtimeStateFrameRegistry.set(key, [runtimeStateSyncEvent(state)]);
      transport.deliver(frameBytes);
    }

    /**
     * Helper: complete initial sync so the engine enters steady state.
     * Sets up handle.receive_frame to route runtime state frames via the registry,
     * and automerge sync frames through the standard initial sync path.
     */
    function setupWithInitialSync(): SyncEngine {
      let callCount = 0;
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockImplementation(
        (bytes: Uint8Array) => {
          // Route based on frame type byte
          const frameType = bytes[0];

          if (frameType === 0x05) {
            // Runtime state sync frame — look up from registry
            const key = Array.from(bytes).join(",");
            const events = runtimeStateFrameRegistry.get(key);
            if (events) return events;
            return [];
          }

          // Automerge sync frame
          callCount++;
          if (callCount === 1) {
            return [syncAppliedEvent({ changed: true })];
          }
          return [syncAppliedEvent({ changed: true })];
        },
      );

      const engine = createEngine();
      engine.start();

      // Complete initial sync
      transport.deliver(Array.from([0x00, 1]));

      return engine;
    }

    it("started transition injects clear changeset into cellChanges$", () => {
      const engine = setupWithInitialSync();

      const emissions: (CellChangeset | null)[] = [];
      engine.cellChanges$.subscribe((cs) => emissions.push(cs));

      // Deliver runtime state with a new "running" execution
      deliverRuntimeState(
        makeRuntimeState({
          e1: { cell_id: "c1", status: "running", execution_count: 1, success: null },
        }),
      );

      // Flush scheduler past coalescing window
      advanceBy(scheduler, 50);

      expect(emissions).toHaveLength(1);
      expect(emissions[0]).not.toBeNull();
      const cs = emissions[0]!;
      expect(cs.changed).toHaveLength(1);
      expect(cs.changed[0].cell_id).toBe("c1");
      expect(cs.changed[0].fields.outputs).toBe(true);
      expect(cs.changed[0].fields.execution_count).toBe(true);
      engine.stop();
    });

    it("done transition injects reconciliation changeset", () => {
      const engine = setupWithInitialSync();

      // Deliver "running" first to establish prev state
      deliverRuntimeState(
        makeRuntimeState({
          e1: { cell_id: "c1", status: "running", execution_count: 1, success: null },
        }),
      );

      // Flush past coalescing to clear the "started" emission
      advanceBy(scheduler, 50);

      const emissions: (CellChangeset | null)[] = [];
      engine.cellChanges$.subscribe((cs) => emissions.push(cs));

      // Now deliver "done"
      deliverRuntimeState(
        makeRuntimeState({
          e1: { cell_id: "c1", status: "done", execution_count: 1, success: true },
        }),
      );

      advanceBy(scheduler, 50);

      expect(emissions).toHaveLength(1);
      expect(emissions[0]).not.toBeNull();
      const cs = emissions[0]!;
      expect(cs.changed).toHaveLength(1);
      expect(cs.changed[0].cell_id).toBe("c1");
      expect(cs.changed[0].fields.outputs).toBe(true);
      expect(cs.changed[0].fields.execution_count).toBe(true);
      engine.stop();
    });

    it("error transition injects reconciliation changeset", () => {
      const engine = setupWithInitialSync();

      // Deliver "running" first to establish prev state
      deliverRuntimeState(
        makeRuntimeState({
          e1: { cell_id: "c1", status: "running", execution_count: 1, success: null },
        }),
      );

      // Flush past coalescing
      advanceBy(scheduler, 50);

      const emissions: (CellChangeset | null)[] = [];
      engine.cellChanges$.subscribe((cs) => emissions.push(cs));

      // Now deliver "error"
      deliverRuntimeState(
        makeRuntimeState({
          e1: { cell_id: "c1", status: "error", execution_count: 1, success: false },
        }),
      );

      advanceBy(scheduler, 50);

      expect(emissions).toHaveLength(1);
      expect(emissions[0]).not.toBeNull();
      const cs = emissions[0]!;
      expect(cs.changed).toHaveLength(1);
      expect(cs.changed[0].cell_id).toBe("c1");
      expect(cs.changed[0].fields.outputs).toBe(true);
      expect(cs.changed[0].fields.execution_count).toBe(true);
      engine.stop();
    });

    it("multiple transitions in one update coalesce into single emission", () => {
      const engine = setupWithInitialSync();

      // Set up prev state with e2 running
      deliverRuntimeState(
        makeRuntimeState({
          e2: { cell_id: "c2", status: "running", execution_count: 1, success: null },
        }),
      );

      // Flush past coalescing
      advanceBy(scheduler, 50);

      const emissions: (CellChangeset | null)[] = [];
      engine.cellChanges$.subscribe((cs) => emissions.push(cs));

      // Deliver update with e1 newly started and e2 done
      deliverRuntimeState(
        makeRuntimeState({
          e1: { cell_id: "c1", status: "running", execution_count: 2, success: null },
          e2: { cell_id: "c2", status: "done", execution_count: 1, success: true },
        }),
      );

      advanceBy(scheduler, 50);

      // Should get a single coalesced emission covering both cells
      expect(emissions).toHaveLength(1);
      expect(emissions[0]).not.toBeNull();
      const cs = emissions[0]!;
      expect(cs.changed).toHaveLength(2);
      const cellIds = cs.changed.map((c) => c.cell_id).sort();
      expect(cellIds).toEqual(["c1", "c2"]);
      engine.stop();
    });

    it("unchanged runtime state does not inject changesets", () => {
      const engine = setupWithInitialSync();

      const state = makeRuntimeState({
        e1: { cell_id: "c1", status: "running", execution_count: 1, success: null },
      });

      // Deliver state first time
      deliverRuntimeState(state);

      // Flush past coalescing to process the first emission
      advanceBy(scheduler, 50);

      const emissions: (CellChangeset | null)[] = [];
      engine.cellChanges$.subscribe((cs) => emissions.push(cs));

      // Deliver same state again — no transitions, no changeset
      deliverRuntimeState(state);

      advanceBy(scheduler, 50);

      expect(emissions).toHaveLength(0);
      engine.stop();
    });

    it("runtime state transitions flow through even before initial sync completes", () => {
      // Set up handle that does NOT complete initial sync on automerge frames
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockImplementation(
        (bytes: Uint8Array) => {
          const frameType = bytes[0];

          if (frameType === 0x05) {
            const key = Array.from(bytes).join(",");
            const events = runtimeStateFrameRegistry.get(key);
            if (events) return events;
            return [];
          }

          // Automerge sync — always changed:false (never completes initial sync)
          return [syncAppliedEvent({ changed: false })];
        },
      );

      const engine = createEngine();
      engine.start();

      // Deliver a sync frame that does NOT complete initial sync
      transport.deliver(Array.from([0x00, 1]));

      const cellEmissions: (CellChangeset | null)[] = [];
      engine.cellChanges$.subscribe((cs) => cellEmissions.push(cs));

      // Deliver runtime state with a transition — this should still inject into cellChanges$
      deliverRuntimeState(
        makeRuntimeState({
          e1: { cell_id: "c1", status: "running", execution_count: 1, success: null },
        }),
      );

      advanceBy(scheduler, 50);

      // Runtime state lifecycle changesets are NOT gated by initial sync
      expect(cellEmissions).toHaveLength(1);
      expect(cellEmissions[0]).not.toBeNull();
      expect(cellEmissions[0]!.changed[0].cell_id).toBe("c1");
      engine.stop();
    });
  });
});

// ── DirectTransport tests ──────────────────────────────────────────

describe("DirectTransport", () => {
  it("delivers frames to subscribers", () => {
    const server = createMockServerHandle();
    const transport = new DirectTransport(server);

    const received: number[][] = [];
    transport.onFrame((payload) => received.push(payload));

    transport.deliver([0x00, 1, 2, 3]);
    expect(received).toHaveLength(1);
    expect(received[0]).toEqual([0x00, 1, 2, 3]);
  });

  it("unsubscribe removes listener", () => {
    const server = createMockServerHandle();
    const transport = new DirectTransport(server);

    const received: number[][] = [];
    const unsub = transport.onFrame((payload) => received.push(payload));

    transport.deliver([1]);
    unsub();
    transport.deliver([2]);

    expect(received).toHaveLength(1);
  });

  it("sendFrame records and routes to server", async () => {
    const server = createMockServerHandle();
    const transport = new DirectTransport(server);

    await transport.sendFrame(FrameType.AUTOMERGE_SYNC, new Uint8Array([1, 2]));

    expect(transport.sentFrames).toHaveLength(1);
    expect(server.receive_sync_message).toHaveBeenCalledWith(new Uint8Array([1, 2]));
  });

  it("simulateFailure causes sendFrame to reject", async () => {
    const server = createMockServerHandle();
    const transport = new DirectTransport(server);

    transport.simulateFailure = true;

    await expect(
      transport.sendFrame(FrameType.AUTOMERGE_SYNC, new Uint8Array([1])),
    ).rejects.toThrow("simulated send failure");

    expect(transport.sendFailureCount).toBe(1);
  });

  it("disconnect prevents further sends", async () => {
    const server = createMockServerHandle();
    const transport = new DirectTransport(server);

    transport.disconnect();
    expect(transport.connected).toBe(false);

    await expect(
      transport.sendFrame(FrameType.AUTOMERGE_SYNC, new Uint8Array([1])),
    ).rejects.toThrow("not connected");
  });

  it("pushBroadcast delivers broadcast frame", () => {
    const server = createMockServerHandle();
    const transport = new DirectTransport(server);

    const received: number[][] = [];
    transport.onFrame((payload) => received.push(payload));

    transport.pushBroadcast({ event: "kernel_status", status: "idle" });

    expect(received).toHaveLength(1);
    expect(received[0][0]).toBe(FrameType.BROADCAST);
  });

  it("clearSentFrames resets history", async () => {
    const server = createMockServerHandle();
    const transport = new DirectTransport(server);

    await transport.sendFrame(FrameType.AUTOMERGE_SYNC, new Uint8Array([1]));
    expect(transport.sentFrames).toHaveLength(1);

    transport.clearSentFrames();
    expect(transport.sentFrames).toHaveLength(0);
  });
});

// ── mergeChangesets (moved from app, verify re-export) ────────────

describe("mergeChangesets", () => {

  it("merges two empty changesets", () => {
    const empty: CellChangeset = {
      changed: [],
      added: [],
      removed: [],
      order_changed: false,
    };
    const result = mergeChangesets(empty, empty);
    expect(result).toEqual(empty);
  });

  it("unions changed fields for the same cell", () => {
    const a: CellChangeset = {
      changed: [{ cell_id: "c1", fields: { source: true } }],
      added: [],
      removed: [],
      order_changed: false,
    };
    const b: CellChangeset = {
      changed: [{ cell_id: "c1", fields: { outputs: true } }],
      added: [],
      removed: [],
      order_changed: false,
    };
    const result = mergeChangesets(a, b);
    expect(result.changed).toHaveLength(1);
    expect(result.changed[0].fields.source).toBe(true);
    expect(result.changed[0].fields.outputs).toBe(true);
  });

  it("deduplicates added/removed", () => {
    const a: CellChangeset = {
      changed: [],
      added: ["c1"],
      removed: ["c2"],
      order_changed: false,
    };
    const b: CellChangeset = {
      changed: [],
      added: ["c1", "c3"],
      removed: ["c2"],
      order_changed: true,
    };
    const result = mergeChangesets(a, b);
    expect(result.added).toEqual(["c1", "c3"]);
    expect(result.removed).toEqual(["c2"]);
    expect(result.order_changed).toBe(true);
  });
});

// ── diffExecutions (moved from app, verify re-export) ────────────

describe("diffExecutions", () => {

  it("detects started transition", () => {
    const prev = {};
    const curr = {
      "e1": { cell_id: "c1", status: "running" as const, execution_count: 1, success: null },
    };
    const transitions = diffExecutions(prev, curr);
    expect(transitions).toHaveLength(1);
    expect(transitions[0].kind).toBe("started");
  });

  it("detects done transition", () => {
    const prev = {
      "e1": { cell_id: "c1", status: "running" as const, execution_count: 1, success: null },
    };
    const curr = {
      "e1": { cell_id: "c1", status: "done" as const, execution_count: 1, success: true },
    };
    const transitions = diffExecutions(prev, curr);
    expect(transitions).toHaveLength(1);
    expect(transitions[0].kind).toBe("done");
  });

  it("detects error transition", () => {
    const prev = {
      "e1": { cell_id: "c1", status: "queued" as const, execution_count: null, success: null },
    };
    const curr = {
      "e1": { cell_id: "c1", status: "error" as const, execution_count: null, success: false },
    };
    const transitions = diffExecutions(prev, curr);
    expect(transitions).toHaveLength(1);
    expect(transitions[0].kind).toBe("error");
  });

  it("returns empty for no change", () => {
    const state = {
      "e1": { cell_id: "c1", status: "running" as const, execution_count: 1, success: null },
    };
    const transitions = diffExecutions(state, state);
    expect(transitions).toHaveLength(0);
  });
});
