/**
 * SyncEngine — transport-agnostic notebook sync engine.
 *
 * Owns all sync state between a local WASM NotebookHandle and the daemon:
 *   - Inbound frame processing (WASM demux → typed events)
 *   - Inline sync reply with rollback on transport failure
 *   - Initial sync handshake with retry
 *   - Coalescing buffer (32ms) for cell changesets
 *   - RuntimeStateDoc sync + execution lifecycle diffing
 *   - Debounced outbound flush of local CRDT mutations
 *
 * Emits typed RxJS observables that consumers subscribe to for
 * materialization, broadcast dispatch, presence routing, etc.
 *
 * Zero Tauri / React / browser dependencies.
 */

import {
  type SchedulerLike,
  bufferTime,
  concatMap,
  debounceTime,
  EMPTY,
  filter,
  from,
  mergeMap,
  Observable,
  share,
  Subject,
  Subscription,
  switchMap,
  timer,
} from "rxjs";

import { type CellChangeset, mergeChangesets } from "./cell-changeset";
import type { FrameEvent, SyncableHandle } from "./handle";
import {
  type ExecutionTransition,
  type RuntimeState,
  type ExecutionState,
  diffExecutions,
} from "./runtime-state";
import { FrameType } from "./transport";
import type { NotebookTransport } from "./transport";

// ── Constants ────────────────────────────────────────────────────────

/** Coalescing window for incoming sync frames (ms). */
const COALESCE_MS = 32;

/** Timeout before retrying sync if initial sync hasn't produced cells (ms). */
const SYNC_RETRY_MS = 3000;

/** Debounce interval for outbound source sync (ms). */
const FLUSH_DEBOUNCE_MS = 20;

// ── Logger interface ─────────────────────────────────────────────────

export interface SyncEngineLogger {
  debug(msg: string, ...args: unknown[]): void;
  info(msg: string, ...args: unknown[]): void;
  warn(msg: string, ...args: unknown[]): void;
  error(msg: string, ...args: unknown[]): void;
}

const nullLogger: SyncEngineLogger = {
  debug() {},
  info() {},
  warn() {},
  error() {},
};

// ── Options ──────────────────────────────────────────────────────────

export interface SyncEngineOptions {
  /**
   * Read the current WASM handle (null during bootstrap).
   *
   * A getter rather than a direct reference so the engine never holds
   * a stale handle across bootstrap cycles.
   */
  getHandle: () => SyncableHandle | null;

  /** Pluggable transport to the daemon. */
  transport: NotebookTransport;

  /** Optional logger (defaults to silent). */
  logger?: SyncEngineLogger;

  /** Optional RxJS scheduler for time-based operators (for testing). */
  scheduler?: SchedulerLike;
}

// ── SyncEngine ───────────────────────────────────────────────────────

export class SyncEngine {
  private readonly opts: Required<Pick<SyncEngineOptions, "getHandle" | "transport" | "logger">> & Pick<SyncEngineOptions, "scheduler">;
  private subscription: Subscription | null = null;
  private awaitingInitialSync = true;
  private prevExecutions: Record<string, ExecutionState> = {};

  // Internal subjects
  private readonly frameIn$ = new Subject<number[]>();
  private readonly flushRequest$ = new Subject<void>();

  // ── Public observables ───────────────────────────────────────────

  /**
   * Coalesced cell changesets from inbound sync frames.
   *
   * Each emission is a merged CellChangeset covering a 32ms window,
   * or `null` when a full materialization is needed (no changeset
   * available from WASM).
   */
  readonly cellChanges$: Observable<CellChangeset | null>;

  /** Daemon broadcast payloads (kernel status, output, env progress, text attributions). */
  readonly broadcasts$: Observable<unknown>;

  /** Remote peer presence updates (cursor, selection, snapshot, left, heartbeat). */
  readonly presence$: Observable<unknown>;

  /** RuntimeState snapshots from the daemon's RuntimeStateDoc. */
  readonly runtimeState$: Observable<RuntimeState>;

  /** Execution lifecycle transitions detected from RuntimeState diffs. */
  readonly executionTransitions$: Observable<ExecutionTransition[]>;

  /**
   * Fires each time the initial sync handshake completes (daemon has
   * sent document content). Emits once per bootstrap cycle — after
   * `resetForBootstrap()`, the next `changed:true` frame triggers
   * another emission. Consumers should do a full materialization in
   * response.
   */
  readonly initialSyncComplete$: Observable<void>;

  // Backing subjects for public observables
  private readonly _cellChanges$ = new Subject<CellChangeset | null>();
  private readonly _broadcasts$ = new Subject<unknown>();
  private readonly _presence$ = new Subject<unknown>();
  private readonly _runtimeState$ = new Subject<RuntimeState>();
  private readonly _executionTransitions$ = new Subject<
    ExecutionTransition[]
  >();
  private readonly _initialSyncComplete$ = new Subject<void>();

  constructor(opts: SyncEngineOptions) {
    this.opts = {
      ...opts,
      logger: opts.logger ?? nullLogger,
      scheduler: opts.scheduler,
    };

    // Expose as readonly Observable (hide Subject internals)
    this.cellChanges$ = this._cellChanges$.asObservable();
    this.broadcasts$ = this._broadcasts$.asObservable();
    this.presence$ = this._presence$.asObservable();
    this.runtimeState$ = this._runtimeState$.asObservable();
    this.executionTransitions$ = this._executionTransitions$.asObservable();
    this.initialSyncComplete$ = this._initialSyncComplete$.asObservable();
  }

  // ── Lifecycle ────────────────────────────────────────────────────

  /**
   * Start processing frames from the transport.
   *
   * Subscribes to the transport's frame listener and wires up all
   * internal RxJS pipelines. Call `stop()` to tear everything down.
   */
  start(): void {
    if (this.subscription) return; // already running
    this.opts.logger.info("[sync-engine] Starting");

    const sub = (this.subscription = new Subscription());
    const log = this.opts.logger;

    // Wire transport frames into the internal subject
    const unlisten = this.opts.transport.onFrame((payload) => {
      this.frameIn$.next(payload);
    });
    sub.add(() => unlisten());

    // Subject for the sync retry timer
    const retrySync$ = new Subject<void>();
    sub.add(() => retrySync$.complete());

    // Arm the retry timer immediately
    retrySync$.next();

    // Subject bridging sync_applied events into the coalescing buffer
    const materialize$ = new Subject<CellChangeset | null>();
    sub.add(() => materialize$.complete());

    // ── Source: frames → WASM demux → individual FrameEvents ──────

    let frameCount = 0;
    let lastFrameLogTime = Date.now();

    const frameEvents$ = this.frameIn$.pipe(
      mergeMap((payload) => {
        try {
          const handle = this.opts.getHandle();
          if (!handle) {
            log.debug("[sync-engine] frame dropped: no handle");
            return EMPTY;
          }
          const bytes = new Uint8Array(payload);
          const result = handle.receive_frame(bytes);
          if (!result || !Array.isArray(result)) return EMPTY;

          // Log frame throughput every 5 seconds
          frameCount++;
          const now = Date.now();
          if (now - lastFrameLogTime >= 5000) {
            log.debug(
              `[sync-engine] ${frameCount} frames processed in ${now - lastFrameLogTime}ms (${bytes.length}B last)`,
            );
            frameCount = 0;
            lastFrameLogTime = now;
          }

          return from(result as FrameEvent[]);
        } catch (e) {
          log.warn("[sync-engine] receive_frame failed:", e);
          return EMPTY;
        }
      }),
      share(),
    );

    // ── Sub-pipeline: sync_applied → initial sync / coalesce ──────

    sub.add(
      frameEvents$
        .pipe(
          filter((e) => e.type === "sync_applied"),
          concatMap((e) => {
            // Attributions → broadcast
            if (e.attributions && e.attributions.length > 0) {
              this._broadcasts$.next({
                type: "text_attribution",
                attributions: e.attributions,
              });
            }

            // Send inline sync reply
            if (e.reply) {
              this.opts.transport
                .sendFrame(
                  FrameType.AUTOMERGE_SYNC,
                  new Uint8Array(e.reply),
                )
                .catch((err: unknown) => {
                  const handle = this.opts.getHandle();
                  if (handle) {
                    handle.cancel_last_flush();
                  }
                  log.warn(
                    "[sync-engine] inline sync reply send failed, rolled back sync state:",
                    err,
                  );
                });
            }

            // Initial sync
            if (this.awaitingInitialSync) {
              if (e.changed) {
                this.awaitingInitialSync = false;
                log.info("[sync-engine] Initial sync complete");
                this._initialSyncComplete$.next();
              } else {
                log.debug("[sync-engine] Initial sync round (awaiting content)");
              }
              // Restart retry timer on handshake rounds
              retrySync$.next();
              return EMPTY;
            }

            // Steady-state: push changeset into coalescing buffer
            if (e.changed) {
              const cs = e.changeset;
              if (cs) {
                log.debug(
                  `[sync-engine] changeset: ${cs.changed.length} changed, ${cs.added.length} added, ${cs.removed.length} removed, order_changed=${cs.order_changed}`,
                );
              } else {
                log.debug(
                  "[sync-engine] sync_applied with change but no changeset (full materialization needed)",
                );
              }
              materialize$.next(cs ?? null);
            }
            return EMPTY;
          }),
        )
        .subscribe(),
    );

    // ── Sync retry timer ──────────────────────────────────────────

    sub.add(
      retrySync$
        .pipe(
          switchMap(() => timer(SYNC_RETRY_MS, this.opts.scheduler)),
          filter(() => this.awaitingInitialSync),
        )
        .subscribe(() => {
          log.info("[sync-engine] Retrying sync after timeout");
          this.resetAndResync();
        }),
    );

    // ── Coalescing buffer → cellChanges$ ──────────────────────────

    sub.add(
      materialize$
        .pipe(
          bufferTime(COALESCE_MS, this.opts.scheduler),
          filter((batch) => batch.length > 0),
          concatMap((batch) => {
            // Merge all changesets in the batch
            let merged: CellChangeset | null = null;
            let needsFull = false;

            for (const cs of batch) {
              if (cs === null) {
                needsFull = true;
              } else if (merged === null) {
                merged = cs;
              } else {
                merged = mergeChangesets(merged, cs);
              }
            }

            const result = needsFull ? null : merged;
            if (needsFull) {
              log.debug(
                `[sync-engine] coalesced ${batch.length} changesets → full materialization`,
              );
            } else if (result) {
              log.debug(
                `[sync-engine] coalesced ${batch.length} changesets → ${result.changed.length} changed, ${result.added.length} added, ${result.removed.length} removed`,
              );
            }
            this._cellChanges$.next(result);
            return EMPTY;
          }),
        )
        .subscribe(),
    );

    // ── Sub-pipeline: broadcasts ──────────────────────────────────

    sub.add(
      frameEvents$
        .pipe(filter((e) => e.type === "broadcast" && e.payload != null))
        .subscribe((e) => this._broadcasts$.next(e.payload)),
    );

    // ── Sub-pipeline: presence ────────────────────────────────────

    sub.add(
      frameEvents$
        .pipe(filter((e) => e.type === "presence" && e.payload != null))
        .subscribe((e) => this._presence$.next(e.payload)),
    );

    // ── Sub-pipeline: sync error recovery ──────────────────────────

    // Notebook doc sync error: send recovery reply + trigger materialization
    sub.add(
      frameEvents$
        .pipe(filter((e) => e.type === "sync_error"))
        .subscribe((e) => {
          log.warn(
            "[sync-engine] sync_error: doc rebuilt, sync state normalized",
          );
          if (e.reply) {
            this.opts.transport
              .sendFrame(
                FrameType.AUTOMERGE_SYNC,
                new Uint8Array(e.reply),
              )
              .catch((err: unknown) => {
                const handle = this.opts.getHandle();
                if (handle) handle.cancel_last_flush();
                log.warn(
                  "[sync-engine] recovery reply send failed:",
                  err,
                );
              });
          }
          // If the doc advanced before the error (partial apply),
          // trigger a full materialization so the UI reflects the
          // recovered state. Also complete initial sync if pending.
          if (e.changed) {
            if (this.awaitingInitialSync) {
              this.awaitingInitialSync = false;
              log.info(
                "[sync-engine] Initial sync completed via error recovery",
              );
              this._initialSyncComplete$.next();
            }
            // null changeset = full materialization needed
            materialize$.next(null);
          }
        }),
    );

    // Runtime state sync error: send recovery reply + publish state
    sub.add(
      frameEvents$
        .pipe(filter((e) => e.type === "runtime_state_sync_error"))
        .subscribe((e) => {
          log.warn(
            "[sync-engine] runtime_state_sync_error: state doc rebuilt, sync state normalized",
          );
          if (e.reply) {
            this.opts.transport
              .sendFrame(
                FrameType.RUNTIME_STATE_SYNC,
                new Uint8Array(e.reply),
              )
              .catch((err: unknown) => {
                const handle = this.opts.getHandle();
                if (handle) handle.cancel_last_runtime_state_flush();
                log.warn(
                  "[sync-engine] state recovery reply send failed:",
                  err,
                );
              });
          }
          // If the state doc advanced, publish the recovered snapshot
          // so kernel status / queue / execution UI stays current.
          if (e.changed && e.state) {
            const state = e.state as RuntimeState;
            const transitions = diffExecutions(
              this.prevExecutions,
              state.executions,
            );
            this.prevExecutions = state.executions;
            this._runtimeState$.next(state);
            if (transitions.length > 0) {
              this._executionTransitions$.next(transitions);
            }
          }
        }),
    );

    // ── Sub-pipeline: runtime state sync ──────────────────────────

    sub.add(
      frameEvents$
        .pipe(
          filter((e) => e.type === "runtime_state_sync_applied"),
          concatMap((e) => {
            if (e.changed && e.state) {
              const state = e.state as RuntimeState;

              // Diff executions for lifecycle transitions
              const transitions = diffExecutions(
                this.prevExecutions,
                state.executions,
              );
              this.prevExecutions = state.executions;

              log.debug(
                `[sync-engine] runtime state: kernel=${state.kernel?.status ?? "?"}, transitions=${transitions.length}`,
              );

              this._runtimeState$.next(state);
              if (transitions.length > 0) {
                this._executionTransitions$.next(transitions);

                // Inject synthetic changesets on execution lifecycle transitions
                // so the materialization pipeline stays in sync with the CRDT.
                //
                // "started": the daemon cleared outputs in the CRDT on
                //   execute_input — re-read from WASM to show empty outputs.
                // "done"/"error": reconcile the store with the CRDT's final
                //   state in case earlier materializations were missed.
                for (const t of transitions) {
                  if (t.kind === "started") {
                    log.debug(
                      `[sync-engine] execution started for ${t.cell_id.slice(0, 8)} — clearing outputs`,
                    );
                  } else {
                    log.debug(
                      `[sync-engine] execution ${t.kind} for ${t.cell_id.slice(0, 8)} — reconciling outputs`,
                    );
                  }
                  materialize$.next({
                    changed: [
                      {
                        cell_id: t.cell_id,
                        fields: { outputs: true, execution_count: true },
                      },
                    ],
                    added: [],
                    removed: [],
                    order_changed: false,
                  });
                }
              }
            }

            // Send sync reply so the daemon knows our heads
            const handle = this.opts.getHandle();
            if (handle) {
              try {
                const reply = handle.generate_runtime_state_sync_reply();
                if (reply) {
                  return from(
                    this.opts.transport
                      .sendFrame(FrameType.RUNTIME_STATE_SYNC, reply)
                      .catch((err: unknown) =>
                        log.warn(
                          "[sync-engine] runtime state sync reply failed:",
                          err,
                        ),
                      ),
                  );
                }
              } catch (err) {
                log.warn(
                  "[sync-engine] generate_runtime_state_sync_reply failed:",
                  err,
                );
              }
            }
            return EMPTY;
          }),
        )
        .subscribe(),
    );

    // ── Debounced outbound flush ──────────────────────────────────

    sub.add(
      this.flushRequest$.pipe(debounceTime(FLUSH_DEBOUNCE_MS, this.opts.scheduler)).subscribe(() => {
        this.flush();
      }),
    );
  }

  /**
   * Stop all pipelines and clean up subscriptions.
   */
  stop(): void {
    if (!this.subscription) return;
    this.opts.logger.info("[sync-engine] Stopping");
    this.subscription.unsubscribe();
    this.subscription = null;
  }

  /** Whether the engine is currently running. */
  get running(): boolean {
    return this.subscription !== null;
  }

  // ── Outbound sync ────────────────────────────────────────────────

  /**
   * Flush local CRDT mutations to the daemon immediately.
   *
   * Sends both the notebook doc sync message and the RuntimeStateDoc
   * sync message. On transport failure, rolls back sync state to
   * prevent the consumption race from #1067.
   */
  flush(): void {
    const handle = this.opts.getHandle();
    if (!handle) {
      this.opts.logger.debug("[sync-engine] flush skipped: no handle");
      return;
    }

    const msg = handle.flush_local_changes();
    if (msg) {
      this.opts.logger.debug(
        `[sync-engine] flushing sync message (${msg.byteLength}B)`,
      );
      this.opts.transport
        .sendFrame(FrameType.AUTOMERGE_SYNC, msg)
        .catch((e: unknown) => {
          handle.cancel_last_flush();
          this.opts.logger.warn(
            "[sync-engine] sync to relay failed:",
            e,
          );
        });
    }

    // Also flush RuntimeStateDoc sync so the daemon sends kernel status,
    // trust state, etc. Without this, if the daemon's initial RuntimeStateSync
    // frame arrived before the WASM handle was ready, the frontend would stay
    // stuck on "not_started" (#runtime-state-race).
    const stateMsg = handle.flush_runtime_state_sync();
    if (stateMsg) {
      this.opts.transport
        .sendFrame(FrameType.RUNTIME_STATE_SYNC, stateMsg)
        .catch((e: unknown) => {
          handle.cancel_last_runtime_state_flush();
          this.opts.logger.warn(
            "[sync-engine] runtime state sync to relay failed:",
            e,
          );
        });
    }
  }

  /**
   * Schedule a debounced flush (for batching rapid keystrokes).
   *
   * Each call resets the 20ms debounce timer. Call `flush()` directly
   * when you need an immediate sync (e.g. before execute or save).
   */
  scheduleFlush(): void {
    this.flushRequest$.next();
  }

  /**
   * Reset sync state and resend the initial sync message.
   *
   * Used when the initial handshake stalls — resets the WASM handle's
   * sync state so `flush_local_changes()` produces a fresh request.
   */
  resetAndResync(): void {
    const handle = this.opts.getHandle();
    if (!handle) return;
    handle.reset_sync_state();
    this.flush();
  }

  /**
   * Reset the engine for a new bootstrap cycle (e.g. daemon:ready).
   *
   * Clears the initial sync gate and execution tracking state so the
   * next round of frames is treated as a fresh connection.
   */
  resetForBootstrap(): void {
    this.opts.logger.info("[sync-engine] Resetting for bootstrap");
    this.awaitingInitialSync = true;
    this.prevExecutions = {};
  }
}
