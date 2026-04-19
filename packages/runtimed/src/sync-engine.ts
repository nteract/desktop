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
  ReplaySubject,
  share,
  Subject,
  Subscription,
  switchMap,
  timer,
} from "rxjs";

import {
  type CommBroadcast,
  type DisplayUpdateBroadcast,
  type KernelErrorBroadcast,
  type OutputBroadcast,
  type OutputsClearedBroadcast,
  isCommBroadcast,
  isDisplayUpdateBroadcast,
  isKernelErrorBroadcast,
  isOutputBroadcast,
  isOutputsClearedBroadcast,
  isRuntimeStateSnapshotBroadcast,
} from "./broadcast-types";
import { type CellChangeset, mergeChangesets } from "./cell-changeset";
import {
  type CommChanges,
  type CommDiffState,
  type ResolvedComm,
  detectUnresolvedOutputs,
  diffComms,
} from "./comm-diff";
import { type KernelStatus, kernelStatus$ as deriveKernelStatus$ } from "./derived-state";
import type { FrameEvent, SyncableHandle } from "./handle";
import type { PoolState } from "./pool-state";
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

// ── Comm state helpers ───────────────────────────────────────────────

/**
 * Backoff delays between text-blob fetch attempts, in milliseconds.
 *
 * Under "run all cells" load the daemon's blob write can race the
 * frontend's GET — especially for large pywidget `_py_render` payloads.
 * Three retries with exponential backoff covers that window without
 * stalling the UI for long on genuinely missing blobs.
 *
 * The first attempt is immediate (no entry). Each subsequent entry is
 * the delay *before* the next attempt.
 */
const TEXT_BLOB_RETRY_DELAYS_MS = [100, 300, 1000];

/** Sleep helper for `inlineTextBlobs` retry backoff. */
function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * For each JSON path in `paths`, read the blob-server URL currently at that
 * position in `state`, fetch its body as text, and replace the URL with the
 * decoded string in place.
 *
 * Used by `projectComms` to resolve text blobs (e.g. anywidget `_py_render`
 * source code) that the WASM resolver left as URL strings — widget code
 * consumes synced string traits directly and can't fetch URLs on its own.
 *
 * Retries transient failures (network errors, 5xx) with exponential
 * backoff; gives up on 4xx (the blob genuinely isn't there). After all
 * attempts fail the URL stays in place and a warning is logged — the
 * widget will render broken, but better than throwing away the whole
 * comm emission.
 */
async function inlineTextBlobs(
  state: Record<string, unknown>,
  paths: string[][],
  logger: SyncEngineLogger,
): Promise<void> {
  if (paths.length === 0) return;
  await Promise.all(
    paths.map(async (path) => {
      const url = readPath(state, path);
      if (typeof url !== "string") return;
      const text = await fetchTextBlobWithRetry(url, logger);
      if (text !== null) {
        writePath(state, path, text);
      }
    }),
  );
}

/**
 * Fetch `url` as text, retrying transient failures.
 *
 * Returns the decoded body on success, or `null` after all retries are
 * exhausted. 4xx responses are treated as permanent and returned
 * immediately without retry (the daemon doesn't know about that hash).
 */
async function fetchTextBlobWithRetry(
  url: string,
  logger: SyncEngineLogger,
): Promise<string | null> {
  let lastReason = "";
  for (let attempt = 0; attempt <= TEXT_BLOB_RETRY_DELAYS_MS.length; attempt++) {
    if (attempt > 0) {
      await delay(TEXT_BLOB_RETRY_DELAYS_MS[attempt - 1]);
    }
    try {
      const res = await fetch(url);
      if (res.ok) {
        return await res.text();
      }
      lastReason = `HTTP ${res.status}`;
      // 4xx is permanent — don't burn retries.
      if (res.status >= 400 && res.status < 500) {
        logger.warn(`[sync-engine] text blob ${url} returned ${res.status}, giving up`);
        return null;
      }
    } catch (err) {
      lastReason = err instanceof Error ? err.message : String(err);
    }
  }
  logger.warn(
    `[sync-engine] text blob ${url} failed after ${TEXT_BLOB_RETRY_DELAYS_MS.length + 1} attempts: ${lastReason}`,
  );
  return null;
}

/** Read `obj[path[0]][path[1]]...` — returns undefined if any step is missing. */
function readPath(obj: unknown, path: string[]): unknown {
  let cursor: unknown = obj;
  for (const seg of path) {
    if (cursor == null || typeof cursor !== "object") return undefined;
    cursor = (cursor as Record<string, unknown>)[seg];
  }
  return cursor;
}

/** Write `value` at `obj[path[0]][path[1]]...`. No-op if the path is missing. */
function writePath(obj: unknown, path: string[], value: unknown): void {
  if (path.length === 0) return;
  let cursor: unknown = obj;
  for (let i = 0; i < path.length - 1; i++) {
    if (cursor == null || typeof cursor !== "object") return;
    cursor = (cursor as Record<string, unknown>)[path[i]];
  }
  if (cursor == null || typeof cursor !== "object") return;
  (cursor as Record<string, unknown>)[path[path.length - 1]] = value;
}

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
  private readonly opts: Required<Pick<SyncEngineOptions, "getHandle" | "transport" | "logger">> &
    Pick<SyncEngineOptions, "scheduler">;
  private subscription: Subscription | null = null;
  private awaitingInitialSync = true;
  private prevExecutions: Record<string, ExecutionState> = {};
  private commDiffState: CommDiffState = { comms: {}, json: {} };
  private lastRuntimeState: RuntimeState | null = null;
  /**
   * Serial queue for async comm emissions.
   *
   * `projectComms` is invoked synchronously from several observable
   * pipelines. Text blobs require HTTP fetches to the blob server, which
   * are async. Chaining each emission's async resolution onto this promise
   * preserves the order of `commChanges$` emissions regardless of which
   * fetch completes first.
   */
  private commEmitQueue: Promise<void> = Promise.resolve();

  // Internal subjects
  private readonly frameIn$ = new Subject<number[]>();
  private readonly flushRequest$ = new Subject<void>();

  /** Promise for the most recent fire-and-forget flush (debounced path). */
  private inflightFlush: Promise<void> | null = null;

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

  /** PoolState snapshots from the daemon's PoolDoc (global pool state). */
  readonly poolState$: Observable<PoolState>;

  /** Execution lifecycle transitions detected from RuntimeState diffs. */
  readonly executionTransitions$: Observable<ExecutionTransition[]>;

  /**
   * Throttled kernel status derived from RuntimeState.
   *
   * Applies a 60ms busy throttle to filter sub-60ms busy→idle blips
   * from tab completions. All other statuses pass through immediately.
   */
  readonly kernelStatus$: Observable<KernelStatus>;

  // ── Typed broadcast observables ──────────────────────────────────

  /** Output produced for a cell (includes manifest hash for blob resolution). */
  readonly outputBroadcasts$: Observable<OutputBroadcast>;

  /** Display data update by display_id. */
  readonly displayUpdates$: Observable<DisplayUpdateBroadcast>;

  /** Outputs cleared for a cell (from another window/peer). */
  readonly outputsCleared$: Observable<OutputsClearedBroadcast>;

  /** Custom comm messages (buttons, model.send()). */
  readonly commBroadcasts$: Observable<CommBroadcast>;

  /** Detailed kernel error message. */
  readonly kernelErrors$: Observable<KernelErrorBroadcast>;

  /**
   * Comm state projection from RuntimeStateDoc.
   *
   * Emits resolved comm lifecycle changes (opened/updated/closed) with
   * ContentRef blobs replaced by URL strings. Subscribers drive their
   * widget store directly — no Jupyter message synthesis needed.
   *
   * Depends on `handle.resolve_comm_state()` (optional on SyncableHandle).
   * If the handle doesn't implement it, this observable never emits.
   */
  readonly commChanges$: Observable<CommChanges>;

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
  private readonly _poolState$ = new Subject<PoolState>();
  private readonly _executionTransitions$ = new Subject<ExecutionTransition[]>();
  private readonly _initialSyncComplete$ = new Subject<void>();
  private readonly _commChanges$ = new Subject<CommChanges>();

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
    this.poolState$ = this._poolState$.asObservable();
    this.executionTransitions$ = this._executionTransitions$.asObservable();
    this.initialSyncComplete$ = this._initialSyncComplete$.asObservable();
    this.commChanges$ = this._commChanges$.asObservable();
    this.kernelStatus$ = deriveKernelStatus$(this.runtimeState$);

    // Typed broadcast sub-observables (derived from broadcasts$)
    this.outputBroadcasts$ = this.broadcasts$.pipe(filter(isOutputBroadcast));
    this.displayUpdates$ = this.broadcasts$.pipe(filter(isDisplayUpdateBroadcast));
    this.outputsCleared$ = this.broadcasts$.pipe(filter(isOutputsClearedBroadcast));
    this.commBroadcasts$ = this.broadcasts$.pipe(filter(isCommBroadcast));
    this.kernelErrors$ = this.broadcasts$.pipe(filter(isKernelErrorBroadcast));
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

    // ReplaySubject(1) so the initial .next() replays to late subscribers.
    // A plain Subject would lose the emission since the retry subscriber
    // is wired up after this point — leaving the retry timer permanently
    // unarmed when no sync frames arrive (see #1417).
    const retrySync$ = new ReplaySubject<void>(1);
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
                .sendFrame(FrameType.AUTOMERGE_SYNC, new Uint8Array(e.reply))
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

    // ── Sub-pipeline: runtime_state_snapshot broadcast ─────────────
    //
    // Eager snapshot from connection setup — apply immediately so the
    // client has kernel status before the Automerge sync handshake completes.

    sub.add(
      this.broadcasts$.pipe(filter(isRuntimeStateSnapshotBroadcast)).subscribe((snapshot) => {
        this._runtimeState$.next(snapshot.state);
        this.projectComms(snapshot.state);
      }),
    );

    // ── Sub-pipeline: sync error recovery ──────────────────────────

    // Notebook doc sync error: send recovery reply + trigger materialization
    sub.add(
      frameEvents$.pipe(filter((e) => e.type === "sync_error")).subscribe((e) => {
        log.warn("[sync-engine] sync_error: doc rebuilt, sync state normalized");
        if (e.reply) {
          this.opts.transport
            .sendFrame(FrameType.AUTOMERGE_SYNC, new Uint8Array(e.reply))
            .catch((err: unknown) => {
              const handle = this.opts.getHandle();
              if (handle) handle.cancel_last_flush();
              log.warn("[sync-engine] recovery reply send failed:", err);
            });
        }
        // If the doc advanced before the error (partial apply),
        // trigger a full materialization so the UI reflects the
        // recovered state. Also complete initial sync if pending.
        if (e.changed) {
          if (this.awaitingInitialSync) {
            this.awaitingInitialSync = false;
            log.info("[sync-engine] Initial sync completed via error recovery");
            this._initialSyncComplete$.next();
          }
          // null changeset = full materialization needed
          materialize$.next(null);
        }
      }),
    );

    // Runtime state sync error: send recovery reply + publish state
    sub.add(
      frameEvents$.pipe(filter((e) => e.type === "runtime_state_sync_error")).subscribe((e) => {
        log.warn(
          "[sync-engine] runtime_state_sync_error: state doc rebuilt, sync state normalized",
        );
        if (e.reply) {
          this.opts.transport
            .sendFrame(FrameType.RUNTIME_STATE_SYNC, new Uint8Array(e.reply))
            .catch((err: unknown) => {
              const handle = this.opts.getHandle();
              if (handle) handle.cancel_last_runtime_state_flush();
              log.warn("[sync-engine] state recovery reply send failed:", err);
            });
        }
        // If the state doc advanced, publish the recovered snapshot
        // so kernel status / queue / execution UI stays current.
        if (e.changed && e.state) {
          const state = e.state as RuntimeState;
          const transitions = diffExecutions(this.prevExecutions, state.executions);
          this.prevExecutions = state.executions;
          this._runtimeState$.next(state);
          if (transitions.length > 0) {
            this._executionTransitions$.next(transitions);
          }
          this.projectComms(state);
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
              const transitions = diffExecutions(this.prevExecutions, state.executions);
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
                // "started": execution_id changed on the cell — WASM facade
                //   returns empty outputs for the new execution_id.
                // "done"/"error": reconcile the store with the final state.
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

              // Output changes detected by WASM-side diff of RuntimeStateDoc.
              // The WASM compares output hash lists before/after sync and
              // reports cell IDs that need re-materialization.
              const outputChangedCells: string[] = e.output_changed_cells ?? [];
              if (outputChangedCells.length > 0) {
                // Deduplicate against cells already handled by transitions
                const transitionCells = new Set(transitions.map((t) => t.cell_id));
                const newOutputCells = outputChangedCells.filter((c) => !transitionCells.has(c));
                if (newOutputCells.length > 0) {
                  log.debug(
                    `[sync-engine] output changes for ${newOutputCells.length} cells from RuntimeStateDoc`,
                  );
                  materialize$.next({
                    changed: newOutputCells.map((cell_id) => ({
                      cell_id,
                      fields: { outputs: true },
                    })),
                    added: [],
                    removed: [],
                    order_changed: false,
                  });
                }
              }

              // ── Comm state projection ──────────────────────────────
              this.projectComms(state);
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
                        log.warn("[sync-engine] runtime state sync reply failed:", err),
                      ),
                  );
                }
              } catch (err) {
                log.warn("[sync-engine] generate_runtime_state_sync_reply failed:", err);
              }
            }
            return EMPTY;
          }),
        )
        .subscribe(),
    );

    // ── Sub-pipeline: pool state sync ─────────────────────────────

    sub.add(
      frameEvents$
        .pipe(
          filter((e) => e.type === "pool_state_sync_applied"),
          concatMap((e) => {
            if (e.changed && e.state) {
              const state = e.state as PoolState;
              this._poolState$.next(state);
            }

            // Send sync reply so the daemon knows our heads
            const handle = this.opts.getHandle();
            if (handle) {
              try {
                const reply = handle.generate_pool_state_sync_reply();
                if (reply) {
                  return from(
                    this.opts.transport
                      .sendFrame(FrameType.POOL_STATE_SYNC, reply)
                      .catch((err: unknown) =>
                        log.warn("[sync-engine] pool state sync reply failed:", err),
                      ),
                  );
                }
              } catch (err) {
                log.warn("[sync-engine] generate_pool_state_sync_reply failed:", err);
              }
            }
            return EMPTY;
          }),
        )
        .subscribe(),
    );

    // Pool state sync error: send recovery reply + publish state
    sub.add(
      frameEvents$.pipe(filter((e) => e.type === "pool_state_sync_error")).subscribe((e) => {
        log.warn("[sync-engine] pool_state_sync_error: pool doc rebuilt, sync state normalized");
        if (e.reply) {
          this.opts.transport
            .sendFrame(FrameType.POOL_STATE_SYNC, new Uint8Array(e.reply))
            .catch((err: unknown) => {
              const handle = this.opts.getHandle();
              if (handle) handle.cancel_last_pool_state_flush();
              log.warn("[sync-engine] pool state recovery reply send failed:", err);
            });
        }
        if (e.changed && e.state) {
          this._poolState$.next(e.state as PoolState);
        }
      }),
    );

    // ── Debounced outbound flush ──────────────────────────────────

    sub.add(
      this.flushRequest$
        .pipe(debounceTime(FLUSH_DEBOUNCE_MS, this.opts.scheduler))
        .subscribe(() => {
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

  // ── Comm state projection ──────────────────────────────────────────

  /**
   * Re-run comm projection against the latest RuntimeState.
   *
   * Call this when blob_port changes — resets diff state so all current
   * comms appear as "opened", then immediately re-projects against the
   * last known state. Without the immediate replay, deferred comms
   * would stay missing until an unrelated runtime-state change arrives.
   */
  reProjectComms(): void {
    this.commDiffState = { comms: {}, json: {} };
    if (this.lastRuntimeState) {
      this.projectComms(this.lastRuntimeState);
    }
  }

  /**
   * Project comm state from a RuntimeState snapshot.
   *
   * Diffs against previous state, resolves ContentRefs via the WASM handle,
   * fetches any text blob references, and emits to commChanges$.
   *
   * The diff computation and `commDiffState` update happen synchronously
   * so successive calls see correct incremental deltas. The final emission
   * is queued on `commEmitQueue` so emissions stay in order even when text
   * blob fetches from one batch outlive a later batch's fetches.
   */
  private projectComms(state: RuntimeState): void {
    this.lastRuntimeState = state;
    const comms = state.comms ?? {};
    const { result, next } = diffComms(this.commDiffState, comms);

    if (result.opened.length === 0 && result.updated.length === 0 && result.closed.length === 0) {
      this.commDiffState = next;
      return;
    }

    const handle = this.opts.getHandle();
    const resolve = (commId: string) =>
      handle?.resolve_comm_state?.(commId) as
        | {
            state: Record<string, unknown>;
            buffer_paths: string[][];
            text_paths?: string[][];
          }
        | undefined;

    // Pending entries carry the raw resolved state plus the text paths that
    // still need to be fetched before emission.
    const opened: Array<{ comm: ResolvedComm; textPaths: string[][] }> = [];
    for (const { commId, entry } of result.opened) {
      const resolved = resolve(commId);
      if (!resolved) {
        // blob_port not ready — defer by excluding from next state.
        // On the next runtimeState$ emission (after blob_port is set),
        // diffComms will see this comm as "new" again and retry.
        delete next.comms[commId];
        delete next.json[commId];
        continue;
      }
      opened.push({
        comm: {
          commId,
          targetName: entry.target_name,
          modelModule: entry.model_module,
          modelName: entry.model_name,
          state: {
            ...resolved.state,
            _model_module: entry.model_module || undefined,
            _model_name: entry.model_name || undefined,
          },
          bufferPaths: resolved.buffer_paths,
          unresolvedOutputs:
            detectUnresolvedOutputs(entry.state as Record<string, unknown>)?.outputs ?? null,
        },
        textPaths: resolved.text_paths ?? [],
      });
    }

    const updated: Array<{ comm: ResolvedComm; textPaths: string[][] }> = [];
    for (const { commId, entry } of result.updated) {
      const resolved = resolve(commId);
      if (!resolved) {
        // resolver not ready (e.g. blob_port transiently missing after
        // reconnect). Revert `next` to the previous state for this comm
        // so the next diff re-surfaces this update instead of swallowing
        // it. Without this revert, `next.json[commId]` would record the
        // new state, and the next projection would see "no change" and
        // never re-emit — the update would be lost permanently until an
        // unrelated future change to the same comm.
        const prevEntry = this.commDiffState.comms[commId];
        const prevJson = this.commDiffState.json[commId];
        if (prevEntry !== undefined && prevJson !== undefined) {
          next.comms[commId] = prevEntry;
          next.json[commId] = prevJson;
        }
        continue;
      }
      updated.push({
        comm: {
          commId,
          targetName: entry.target_name,
          modelModule: entry.model_module,
          modelName: entry.model_name,
          state: resolved.state,
          bufferPaths: resolved.buffer_paths,
          unresolvedOutputs:
            detectUnresolvedOutputs(entry.state as Record<string, unknown>)?.outputs ?? null,
        },
        textPaths: resolved.text_paths ?? [],
      });
    }

    this.commDiffState = next;

    if (opened.length === 0 && updated.length === 0 && result.closed.length === 0) {
      return;
    }

    // Serialize async resolution + emit so ordering is preserved across
    // overlapping projectComms calls. A `.catch` keeps one failing fetch
    // from poisoning the queue for subsequent batches.
    const log = this.opts.logger;
    this.commEmitQueue = this.commEmitQueue
      .then(async () => {
        await Promise.all([
          ...opened.map((o) => inlineTextBlobs(o.comm.state, o.textPaths, log)),
          ...updated.map((u) => inlineTextBlobs(u.comm.state, u.textPaths, log)),
        ]);
        this._commChanges$.next({
          opened: opened.map((o) => o.comm),
          updated: updated.map((u) => u.comm),
          closed: result.closed,
        });
      })
      .catch((err) => {
        log.warn("[sync-engine] comm emission failed:", err);
      });
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
      this.opts.logger.debug(`[sync-engine] flushing sync message (${msg.byteLength}B)`);
      const done = this.opts.transport
        .sendFrame(FrameType.AUTOMERGE_SYNC, msg)
        .catch((e: unknown) => {
          handle.cancel_last_flush();
          this.opts.logger.warn("[sync-engine] sync to relay failed:", e);
        });
      // Track the in-flight flush so flushAndWait() can await it.
      this.inflightFlush = done;
    }

    // Also flush RuntimeStateDoc sync so the daemon sends kernel status,
    // trust state, etc. Without this, if the daemon's initial RuntimeStateSync
    // frame arrived before the WASM handle was ready, the frontend would stay
    // stuck on "not_started" (#runtime-state-race).
    const stateMsg = handle.flush_runtime_state_sync();
    if (stateMsg) {
      this.opts.transport.sendFrame(FrameType.RUNTIME_STATE_SYNC, stateMsg).catch((e: unknown) => {
        handle.cancel_last_runtime_state_flush();
        this.opts.logger.warn("[sync-engine] runtime state sync to relay failed:", e);
      });
    }

    // Also flush PoolDoc sync so the daemon sends pool state.
    const poolMsg = handle.flush_pool_state_sync();
    if (poolMsg) {
      this.opts.transport.sendFrame(FrameType.POOL_STATE_SYNC, poolMsg).catch((e: unknown) => {
        handle.cancel_last_pool_state_flush();
        this.opts.logger.warn("[sync-engine] pool state sync to relay failed:", e);
      });
    }
  }

  /**
   * Flush local changes and wait for delivery.
   *
   * Unlike `flush()` (fire-and-forget), this method:
   * 1. Awaits any in-flight debounced flush that may have already claimed
   *    changes from `flush_local_changes()`.
   * 2. Flushes any remaining local changes and awaits delivery.
   *
   * Use before execute/save to guarantee the daemon has the latest source.
   */
  async flushAndWait(): Promise<void> {
    // Drain all in-flight debounced flushes. A new debounced flush can
    // start while we're awaiting the current one (the 20ms timer fires
    // independently), so loop until stable.
    while (this.inflightFlush) {
      const current = this.inflightFlush;
      await current;
      // Only clear if no newer flush replaced it while we awaited.
      if (this.inflightFlush === current) {
        this.inflightFlush = null;
      }
    }

    const handle = this.opts.getHandle();
    if (!handle) return;

    // Flush any remaining notebook doc changes (may be none if debounce got them).
    const msg = handle.flush_local_changes();
    if (msg) {
      this.opts.logger.debug(`[sync-engine] flushAndWait: sending ${msg.byteLength}B sync message`);
      try {
        await this.opts.transport.sendFrame(FrameType.AUTOMERGE_SYNC, msg);
      } catch (e) {
        handle.cancel_last_flush();
        this.opts.logger.warn("[sync-engine] flushAndWait: sync to relay failed:", e);
      }
    }

    // Also flush RuntimeStateDoc sync.
    const stateMsg = handle.flush_runtime_state_sync();
    if (stateMsg) {
      try {
        await this.opts.transport.sendFrame(FrameType.RUNTIME_STATE_SYNC, stateMsg);
      } catch (e) {
        handle.cancel_last_runtime_state_flush();
        this.opts.logger.warn("[sync-engine] flushAndWait: runtime state sync failed:", e);
      }
    }

    // Also flush PoolDoc sync.
    const poolMsg = handle.flush_pool_state_sync();
    if (poolMsg) {
      try {
        await this.opts.transport.sendFrame(FrameType.POOL_STATE_SYNC, poolMsg);
      } catch (e) {
        handle.cancel_last_pool_state_flush();
        this.opts.logger.warn("[sync-engine] flushAndWait: pool state sync failed:", e);
      }
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
    this.commDiffState = { comms: {}, json: {} };
    this.lastRuntimeState = null;
  }
}
