/**
 * WASM test harness — loads real runtimed-wasm in vitest, providing
 * a server (daemon) + client pair connected via DirectTransport.
 *
 * This lets us generate *real* Automerge frames from one handle and
 * pump them through SyncEngine on the other side, testing the actual
 * sync → changeset → materialization pipeline without mocks.
 *
 * NOTE: The WASM NotebookHandle does not expose output-writing methods
 * (append_output, set_outputs). Those are daemon-only Rust APIs. Tests
 * that need output changes should use the mock-based approach instead.
 * This harness is for testing sync, changesets, source edits, metadata,
 * structural changes, and concurrent edit resolution.
 */

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { VirtualAction, VirtualTimeScheduler } from "rxjs";
import type { NotebookHandle } from "../../../apps/notebook/src/wasm/runtimed-wasm/runtimed_wasm";
import { DirectTransport, type ServerHandle } from "../src/direct-transport";
import type { SyncableHandle } from "../src/handle";
import { SyncEngine } from "../src/sync-engine";

// ── WASM initialization ──────────────────────────────────────────────

let wasmInitialized = false;
let WasmNotebookHandle: typeof NotebookHandle;

/**
 * Load and initialize the WASM module once. Subsequent calls are no-ops.
 */
export async function initWasm(): Promise<typeof NotebookHandle> {
  if (wasmInitialized) return WasmNotebookHandle;

  const wasmPath = resolve(
    __dirname,
    "../../../apps/notebook/src/wasm/runtimed-wasm/runtimed_wasm_bg.wasm",
  );
  const jsPath = resolve(
    __dirname,
    "../../../apps/notebook/src/wasm/runtimed-wasm/runtimed_wasm.js",
  );

  const wasmBuffer = readFileSync(wasmPath);
  // Convert Node Buffer → ArrayBuffer for WebAssembly.Module
  const wasmBytes = wasmBuffer.buffer.slice(
    wasmBuffer.byteOffset,
    wasmBuffer.byteOffset + wasmBuffer.byteLength,
  );
  const mod = await import(jsPath);
  mod.initSync({ module: new Uint8Array(wasmBytes) });

  wasmInitialized = true;
  WasmNotebookHandle = mod.NotebookHandle;
  return WasmNotebookHandle;
}

// ── Server handle adapter ────────────────────────────────────────────

/**
 * Adapts a real NotebookHandle into the ServerHandle interface needed
 * by DirectTransport.
 */
class WasmServerHandle implements ServerHandle {
  readonly handle: NotebookHandle;

  constructor(handle: NotebookHandle) {
    this.handle = handle;
  }

  flush_local_changes(): Uint8Array | null {
    return this.handle.flush_local_changes() ?? null;
  }

  receive_sync_message(message: Uint8Array): boolean {
    return this.handle.receive_sync_message(message);
  }

  reset_sync_state(): void {
    this.handle.reset_sync_state();
  }

  flush_runtime_state_sync(): Uint8Array | null {
    return this.handle.flush_runtime_state_sync() ?? null;
  }

  receive_frame(frame: Uint8Array): unknown {
    return this.handle.receive_frame(frame);
  }
}

// ── Test harness ─────────────────────────────────────────────────────

export interface WasmHarness {
  /** The "daemon" handle — mutate this to generate frames. */
  server: NotebookHandle;
  /** The "client" handle — receives frames via SyncEngine. */
  client: NotebookHandle;
  /** The SyncEngine wired to the client handle. */
  engine: SyncEngine;
  /** The transport connecting server → client. */
  transport: DirectTransport;
  /** VirtualTimeScheduler for controlling time in tests. */
  scheduler: VirtualTimeScheduler;

  /** Add a cell on the server (daemon) side. */
  serverAddCell(cellId: string, cellType: string, afterIndex?: number): void;
  /** Update cell source on the server. */
  serverUpdateSource(cellId: string, source: string): void;
  /** Set cell execution count on the server. */
  serverSetExecutionCount(cellId: string, count: string): void;
  /** Clear outputs for a cell on the server. */
  serverClearOutputs(cellId: string): void;

  /**
   * Open a comm on the server (daemon) side. Mirrors the daemon's
   * reaction to a kernel `comm_open` IOPub message — creates the
   * `doc.comms.<commId>` entry with metadata + initial state.
   */
  serverOpenComm(
    commId: string,
    opts: {
      targetName: string;
      modelModule: string;
      modelName: string;
      state: Record<string, unknown>;
      seq?: number;
    },
  ): void;

  /**
   * Apply a widget-state patch on the server (daemon) side. The comm
   * must already be opened via `serverOpenComm`. Mirrors how the real
   * daemon writes kernel `comm_msg(update)` traffic into
   * `doc.comms.<commId>.state`.
   */
  serverSetCommState(commId: string, patch: Record<string, unknown>): void;

  /**
   * Push server changes to the client via the transport.
   * Returns true if a sync message was delivered.
   */
  pushToClient(): boolean;

  /**
   * Push the server's pending RuntimeStateDoc changes to the client.
   * Returns true if a sync message was delivered.
   */
  pushRuntimeStateToClient(): boolean;

  /**
   * Run RuntimeStateDoc sync rounds between server and client until
   * convergence. Each round: push server's pending state → client
   * applies → client's reply goes back to server. Yields microtasks
   * between rounds so the engine's async comm-emit queue can drain.
   */
  syncRuntimeState(maxRounds?: number): Promise<number>;

  /**
   * Run sync rounds between server and client until converged.
   * Returns the number of rounds.
   */
  syncUntilConverged(maxRounds?: number): number;

  /**
   * Start the engine and complete the initial sync handshake.
   *
   * Handles the full multi-round Automerge sync protocol:
   * 1. Engine starts and flushes its initial sync request to server
   * 2. Server responds with its document state
   * 3. Client processes response, completes initial sync
   */
  startAndCompleteSync(): Promise<void>;

  /**
   * Advance the virtual clock by `ms` milliseconds.
   * Use this to flush the 32ms coalescing buffer.
   */
  advanceBy(ms: number): void;

  /**
   * Push server changes + advance past coalescing window.
   * This is the common pattern for "make a change and see it".
   */
  pushAndFlush(): void;

  /** Tear down handles and engine. */
  dispose(): void;
}

export async function createWasmHarness(notebookId = "test-notebook"): Promise<WasmHarness> {
  const Handle = await initWasm();

  // Server = daemon side (creates the doc, adds cells)
  const serverHandle = new Handle(notebookId);

  // Client = frontend side (receives sync frames, produces changesets)
  // Use create_bootstrap for sync-only mode (like the real frontend)
  const clientHandle = Handle.create_bootstrap(`test-client-${Date.now()}`);
  // Set a dummy blob port so comm-state resolution doesn't defer comms
  // waiting for it. Tests that care about blob URLs can override by
  // calling `client.set_blob_port(realPort)` before syncing.
  clientHandle.set_blob_port(1);

  const serverAdapter = new WasmServerHandle(serverHandle);
  const transport = new DirectTransport(serverAdapter);
  const scheduler = new VirtualTimeScheduler(VirtualAction, Infinity);

  const engine = new SyncEngine({
    getHandle: () => clientHandle as unknown as SyncableHandle,
    transport,
    scheduler,
  });

  function advanceBy(ms: number): void {
    const target = scheduler.frame + ms;
    scheduler.maxFrames = target;
    scheduler.schedule(() => {}, ms);
    scheduler.flush();
  }

  const harness: WasmHarness = {
    server: serverHandle,
    client: clientHandle,
    engine,
    transport,
    scheduler,

    serverAddCell(cellId: string, cellType: string, afterIndex?: number) {
      serverHandle.add_cell(afterIndex ?? serverHandle.cell_count(), cellId, cellType);
    },

    serverUpdateSource(cellId: string, source: string) {
      serverHandle.update_source(cellId, source);
    },

    serverSetExecutionCount(_cellId: string, _count: string) {
      // No-op: execution_count is now in RuntimeStateDoc, not NotebookDoc.
      // The WASM set_execution_count method was removed. Tests that need
      // to verify execution_count should use RuntimeStateDoc instead.
    },

    serverClearOutputs(cellId: string) {
      serverHandle.clear_outputs(cellId);
    },

    serverOpenComm(commId, opts) {
      serverHandle.put_comm_for_test(
        commId,
        opts.targetName,
        opts.modelModule,
        opts.modelName,
        JSON.stringify(opts.state),
        opts.seq ?? 0,
      );
    },

    serverSetCommState(commId: string, patch: Record<string, unknown>) {
      serverHandle.set_comm_state_batch(commId, JSON.stringify(patch));
    },

    pushToClient(): boolean {
      return transport.pushServerChanges();
    },

    pushRuntimeStateToClient(): boolean {
      return transport.pushServerRuntimeStateChanges();
    },

    async syncRuntimeState(maxRounds = 8): Promise<number> {
      let rounds = 0;
      for (let i = 0; i < maxRounds; i++) {
        const pushed = transport.pushServerRuntimeStateChanges();
        // Yield a few microtasks so the engine's async comm-emit queue
        // can settle (projectComms chains on Promise.resolve().then).
        for (let j = 0; j < 4; j++) {
          await Promise.resolve();
        }
        // Also call the engine's flush, which sends the client's sync
        // reply back to the server — the server needs it before it can
        // generate its next message.
        await engine.flush();
        for (let j = 0; j < 4; j++) {
          await Promise.resolve();
        }
        if (!pushed) break;
        rounds++;
      }
      return rounds;
    },

    syncUntilConverged(maxRounds = 10): number {
      return transport.syncUntilConverged(maxRounds);
    },

    async startAndCompleteSync(): Promise<void> {
      engine.start();

      const syncComplete = new Promise<void>((resolve) => {
        const sub = engine.initialSyncComplete$.subscribe(() => {
          sub.unsubscribe();
          resolve();
        });
      });

      // Kick off the handshake: client → server → client
      engine.flush();

      // Run sync rounds until the handshake completes.
      // Each round: push server response → engine processes → may generate reply
      for (let i = 0; i < 20; i++) {
        const pushed = transport.pushServerChanges();
        if (!pushed) break;
        // Give the engine a tick to process
        await Promise.resolve();
      }

      await syncComplete;
    },

    advanceBy,

    pushAndFlush() {
      transport.pushServerChanges();
      // Advance past the 32ms coalescing window
      advanceBy(50);
    },

    dispose() {
      engine.stop();
      serverHandle.free();
      clientHandle.free();
    },
  };

  return harness;
}
