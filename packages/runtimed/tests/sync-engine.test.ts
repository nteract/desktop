/**
 * SyncEngine unit tests using mock handles.
 *
 * Proves the engine's lifecycle, coalescing, rollback, retry, and
 * observable emission without requiring WASM or a real daemon.
 */

import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { firstValueFrom } from "rxjs";
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

const EMPTY_CHANGESET: CellChangeset = {
  changed: [],
  added: [],
  removed: [],
  order_changed: false,
};

// ── Tests ────────────────────────────────────────────────────────────

describe("SyncEngine", () => {
  let handle: SyncableHandle;
  let server: ReturnType<typeof createMockServerHandle>;
  let transport: DirectTransport;
  let engine: SyncEngine;

  beforeEach(() => {
    vi.useFakeTimers();
    handle = createMockHandle();
    server = createMockServerHandle();
    transport = new DirectTransport(server);
    engine = new SyncEngine({
      getHandle: () => handle,
      transport,
    });
  });

  afterEach(() => {
    engine.stop();
    vi.useRealTimers();
  });

  // ── Lifecycle ──────────────────────────────────────────────────

  describe("lifecycle", () => {
    it("starts and stops cleanly", () => {
      expect(engine.running).toBe(false);
      engine.start();
      expect(engine.running).toBe(true);
      engine.stop();
      expect(engine.running).toBe(false);
    });

    it("start is idempotent", () => {
      engine.start();
      engine.start(); // should not throw or double-subscribe
      expect(engine.running).toBe(true);
    });

    it("stop is idempotent", () => {
      engine.start();
      engine.stop();
      engine.stop(); // should not throw
      expect(engine.running).toBe(false);
    });
  });

  // ── Initial sync ──────────────────────────────────────────────

  describe("initial sync", () => {
    it("emits initialSyncComplete$ when changed:true arrives", async () => {
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        syncAppliedEvent({ changed: true }),
      ]);

      engine.start();

      const completed = firstValueFrom(engine.initialSyncComplete$);
      transport.deliver(Array.from([0x00, 1, 2, 3])); // dummy frame
      await completed; // should resolve
    });

    it("does not emit initialSyncComplete$ on changed:false", async () => {
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        syncAppliedEvent({ changed: false }),
      ]);

      engine.start();

      let completed = false;
      engine.initialSyncComplete$.subscribe(() => {
        completed = true;
      });

      transport.deliver(Array.from([0x00, 1, 2, 3]));
      await vi.advanceTimersByTimeAsync(100);
      expect(completed).toBe(false);
    });

    it("retries sync after timeout when initial sync stalls", async () => {
      // First frame: changed:false (handshake, no content)
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        syncAppliedEvent({ changed: false }),
      ]);

      // flush_local_changes returns a sync message for the retry
      (handle.flush_local_changes as ReturnType<typeof vi.fn>).mockReturnValue(
        new Uint8Array([1, 2, 3]),
      );

      engine.start();
      transport.deliver(Array.from([0x00, 1, 2, 3]));

      // Advance past the 3s retry timeout
      await vi.advanceTimersByTimeAsync(3100);

      // Engine should have called reset_sync_state + flush for retry
      expect(handle.reset_sync_state).toHaveBeenCalled();
    });
  });

  // ── Broadcasts ────────────────────────────────────────────────

  describe("broadcasts", () => {
    it("emits broadcast payloads on broadcasts$", async () => {
      const broadcastPayload = { event: "kernel_status", status: "busy" };
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        broadcastEvent(broadcastPayload),
      ]);

      engine.start();

      const received = firstValueFrom(engine.broadcasts$);
      transport.deliver(Array.from([0x03, 1])); // dummy frame
      const payload = await received;
      expect(payload).toEqual(broadcastPayload);
    });

    it("emits text_attribution as broadcast", async () => {
      const attributions = [
        { cell_id: "c1", index: 0, text: "hi", deleted: 0, actors: ["a"] },
      ];
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        syncAppliedEvent({ changed: true, attributions }),
      ]);

      engine.start();

      const received = firstValueFrom(engine.broadcasts$);
      transport.deliver(Array.from([0x00, 1]));
      const payload = await received;
      expect(payload).toEqual({
        type: "text_attribution",
        attributions,
      });
    });
  });

  // ── Presence ──────────────────────────────────────────────────

  describe("presence", () => {
    it("emits presence payloads on presence$", async () => {
      const presencePayload = { type: "update", peer: "alice", cursor: {} };
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        presenceEvent(presencePayload),
      ]);

      engine.start();

      const received = firstValueFrom(engine.presence$);
      transport.deliver(Array.from([0x04, 1]));
      const payload = await received;
      expect(payload).toEqual(presencePayload);
    });
  });

  // ── Cell changes (coalescing) ─────────────────────────────────

  describe("cellChanges$", () => {
    it("emits coalesced changesets after initial sync", async () => {
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

      engine.start();

      // Complete initial sync
      transport.deliver(Array.from([0x00, 1]));
      await vi.advanceTimersByTimeAsync(0);

      // Subscribe to cell changes
      const changePromise = firstValueFrom(engine.cellChanges$);

      // Send steady-state frame
      transport.deliver(Array.from([0x00, 2]));

      // Advance past coalescing window (32ms)
      await vi.advanceTimersByTimeAsync(50);

      const changeset = await changePromise;
      expect(changeset).not.toBeNull();
      expect(changeset!.changed[0].cell_id).toBe("c1");
      expect(changeset!.changed[0].fields.source).toBe(true);
    });

    it("emits null changeset when WASM has no changeset", async () => {
      let callCount = 0;
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockImplementation(() => {
        callCount++;
        if (callCount === 1) {
          return [syncAppliedEvent({ changed: true })];
        }
        return [syncAppliedEvent({ changed: true })]; // no changeset
      });

      engine.start();

      // Complete initial sync
      transport.deliver(Array.from([0x00, 1]));
      await vi.advanceTimersByTimeAsync(0);

      const changePromise = firstValueFrom(engine.cellChanges$);
      transport.deliver(Array.from([0x00, 2]));
      await vi.advanceTimersByTimeAsync(50);

      const changeset = await changePromise;
      expect(changeset).toBeNull();
    });
  });

  // ── Runtime state ─────────────────────────────────────────────

  describe("runtimeState$", () => {
    it("emits runtime state on state sync", async () => {
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

      engine.start();

      const received = firstValueFrom(engine.runtimeState$);
      transport.deliver(Array.from([0x05, 1]));
      const result = await received;
      expect(result.kernel.status).toBe("busy");
      expect(result.kernel.name).toBe("python3");
    });
  });

  // ── Execution transitions ─────────────────────────────────────

  describe("executionTransitions$", () => {
    it("detects started transition", async () => {
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

      engine.start();

      const received = firstValueFrom(engine.executionTransitions$);
      transport.deliver(Array.from([0x05, 1]));
      const transitions = await received;
      expect(transitions).toHaveLength(1);
      expect(transitions[0].kind).toBe("started");
      expect(transitions[0].cell_id).toBe("c1");
      expect(transitions[0].execution_id).toBe("exec-1");
    });
  });

  // ── Inline sync reply ─────────────────────────────────────────

  describe("sync replies", () => {
    it("sends inline sync reply via transport", () => {
      const reply = [10, 20, 30];
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        syncAppliedEvent({ changed: true, reply }),
      ]);

      engine.start();
      transport.deliver(Array.from([0x00, 1]));

      // Check that a sync frame was sent
      const syncFrames = transport.sentFrames.filter(
        (f) => f.frameType === FrameType.AUTOMERGE_SYNC,
      );
      expect(syncFrames.length).toBeGreaterThanOrEqual(1);
    });

    it("rolls back sync state on send failure", async () => {
      const reply = [10, 20, 30];
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        syncAppliedEvent({ changed: true, reply }),
      ]);

      transport.simulateFailure = true;
      engine.start();
      transport.deliver(Array.from([0x00, 1]));

      // Let the promise rejection propagate
      await vi.advanceTimersByTimeAsync(0);

      expect(handle.cancel_last_flush).toHaveBeenCalled();
    });
  });

  // ── Outbound flush ────────────────────────────────────────────

  describe("flush", () => {
    it("flush() sends local changes via transport", () => {
      const syncMsg = new Uint8Array([1, 2, 3]);
      (handle.flush_local_changes as ReturnType<typeof vi.fn>).mockReturnValue(syncMsg);

      engine.start();
      engine.flush();

      const syncFrames = transport.sentFrames.filter(
        (f) => f.frameType === FrameType.AUTOMERGE_SYNC,
      );
      expect(syncFrames).toHaveLength(1);
      expect(syncFrames[0].payload).toEqual(syncMsg);
    });

    it("flush() also sends RuntimeStateDoc sync", () => {
      const stateMsg = new Uint8Array([4, 5, 6]);
      (handle.flush_runtime_state_sync as ReturnType<typeof vi.fn>).mockReturnValue(stateMsg);

      engine.start();
      engine.flush();

      const stateFrames = transport.sentFrames.filter(
        (f) => f.frameType === FrameType.RUNTIME_STATE_SYNC,
      );
      expect(stateFrames).toHaveLength(1);
      expect(stateFrames[0].payload).toEqual(stateMsg);
    });

    it("flush() rolls back on transport failure", async () => {
      const syncMsg = new Uint8Array([1, 2, 3]);
      (handle.flush_local_changes as ReturnType<typeof vi.fn>).mockReturnValue(syncMsg);

      transport.simulateFailure = true;
      engine.start();
      engine.flush();

      await vi.advanceTimersByTimeAsync(0);
      expect(handle.cancel_last_flush).toHaveBeenCalled();
    });

    it("scheduleFlush() debounces at 20ms", async () => {
      const syncMsg = new Uint8Array([1]);
      (handle.flush_local_changes as ReturnType<typeof vi.fn>).mockReturnValue(syncMsg);

      engine.start();
      engine.scheduleFlush();
      engine.scheduleFlush();
      engine.scheduleFlush();

      // No flush yet
      expect(transport.sentFrames).toHaveLength(0);

      // Advance past debounce (20ms)
      await vi.advanceTimersByTimeAsync(25);

      // Should have flushed exactly once
      const syncFrames = transport.sentFrames.filter(
        (f) => f.frameType === FrameType.AUTOMERGE_SYNC,
      );
      expect(syncFrames).toHaveLength(1);
    });
  });

  // ── resetAndResync ────────────────────────────────────────────

  describe("resetAndResync", () => {
    it("resets sync state and flushes", () => {
      const syncMsg = new Uint8Array([7, 8, 9]);
      (handle.flush_local_changes as ReturnType<typeof vi.fn>).mockReturnValue(syncMsg);

      engine.start();
      engine.resetAndResync();

      expect(handle.reset_sync_state).toHaveBeenCalled();
      expect(transport.sentFrames.length).toBeGreaterThanOrEqual(1);
    });
  });

  // ── resetForBootstrap ─────────────────────────────────────────

  describe("resetForBootstrap", () => {
    it("emits initialSyncComplete$ again after resetForBootstrap + changed:true", async () => {
      (handle.receive_frame as ReturnType<typeof vi.fn>).mockReturnValue([
        syncAppliedEvent({ changed: true }),
      ]);

      engine.start();

      // Track all emissions
      let emitCount = 0;
      engine.initialSyncComplete$.subscribe(() => {
        emitCount++;
      });

      // Complete first initial sync
      transport.deliver(Array.from([0x00, 1]));
      await vi.advanceTimersByTimeAsync(0);
      expect(emitCount).toBe(1);

      // Simulate daemon:ready — reset for a new bootstrap cycle
      engine.resetForBootstrap();

      // Second initial sync should emit again
      transport.deliver(Array.from([0x00, 2]));
      await vi.advanceTimersByTimeAsync(0);
      expect(emitCount).toBe(2);
    });

    it("does not emit cellChanges$ during initial sync phase", async () => {
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

      engine.start();

      let cellChangeCount = 0;
      engine.cellChanges$.subscribe(() => {
        cellChangeCount++;
      });

      // First frame completes initial sync — should NOT go to cellChanges$
      transport.deliver(Array.from([0x00, 1]));
      await vi.advanceTimersByTimeAsync(50);
      expect(cellChangeCount).toBe(0);

      // After initial sync, steady-state frames go to cellChanges$
      transport.deliver(Array.from([0x00, 2]));
      await vi.advanceTimersByTimeAsync(50);
      expect(cellChangeCount).toBe(1);

      // Reset for bootstrap — back to initial sync phase
      engine.resetForBootstrap();

      // This frame should NOT go to cellChanges$ (awaiting initial sync)
      transport.deliver(Array.from([0x00, 3]));
      await vi.advanceTimersByTimeAsync(50);
      expect(cellChangeCount).toBe(1); // unchanged
    });
  });

  // ── Null handle safety ────────────────────────────────────────

  describe("null handle", () => {
    it("does not crash when handle is null", () => {
      const nullEngine = new SyncEngine({
        getHandle: () => null,
        transport,
      });

      nullEngine.start();
      transport.deliver(Array.from([0x00, 1, 2, 3]));
      nullEngine.flush();
      nullEngine.scheduleFlush();
      nullEngine.resetAndResync();
      nullEngine.stop();
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
