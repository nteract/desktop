/**
 * `TauriTransport` — `NotebookTransport` implementation for the Tauri desktop app.
 *
 * Bridges the `runtimed` `SyncEngine` to the daemon via Tauri IPC:
 *   - `sendFrame` → `invoke("send_frame", bytes)`
 *   - `onFrame` → `getCurrentWebview().listen("notebook:frame")`
 *   - `sendRequest` → encode `NotebookRequestEnvelope`, send as `0x01` frame,
 *                     wait for matching `0x02` response via an internal
 *                     frame tap keyed by correlation id.
 *
 * The pending map is tapped directly off `notebook:frame` events so Rust
 * and JS callers can share the same socket concurrently — responses are
 * dispatched by id, not by serialization order.
 */

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import {
  FrameType,
  type FrameListener,
  type NotebookRequest,
  type NotebookRequestOptions,
  type NotebookResponse,
  type NotebookTransport,
} from "runtimed";

const FRAME_TYPE_REQUEST = 0x01;
const FRAME_TYPE_RESPONSE = 0x02;

/** Per-request-type timeouts. Mirror `relay_task::request_timeout` in Rust. */
function requestTimeoutMs(request: NotebookRequest): number {
  switch (request.type) {
    case "launch_kernel":
    case "sync_environment":
      return 300_000;
    case "complete":
      return 7_000;
    default:
      return 30_000;
  }
}

interface PendingEntry {
  resolve: (response: NotebookResponse) => void;
  reject: (err: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

export class TauriTransport implements NotebookTransport {
  private _connected = true;
  private unlisteners: Array<() => void> = [];
  /** In-flight requests keyed by correlation id. */
  private pending = new Map<string, PendingEntry>();

  constructor() {
    this.attachResponseTap();
  }

  get connected(): boolean {
    return this._connected;
  }

  async sendFrame(frameType: number, payload: Uint8Array): Promise<void> {
    if (frameType === FrameType.SESSION_CONTROL) {
      throw new Error("SESSION_CONTROL is server-originated only");
    }
    const frame = new Uint8Array(1 + payload.length);
    frame[0] = frameType;
    frame.set(payload, 1);
    return invoke("send_frame", frame);
  }

  onFrame(callback: FrameListener): () => void {
    const webview = getCurrentWebview();

    let unlistenFn: (() => void) | null = null;
    let cancelled = false;

    // IMPORTANT: wrap the callback in try/catch. Tauri's event system drops
    // listeners whose handlers throw — a single exception escaping here
    // silently unsubscribes the webview for the rest of its lifetime, and
    // the daemon's subsequent frames land nowhere. Catching preserves the
    // listener across a bad frame and surfaces the exception to the console
    // so the underlying issue is fixable.
    const unlistenPromise = webview.listen<number[]>("notebook:frame", (event) => {
      try {
        callback(event.payload);
      } catch (err) {
        console.error("[tauri-transport] notebook:frame handler threw:", err);
      }
    });

    unlistenPromise
      .then((fn) => {
        if (cancelled) {
          fn();
        } else {
          unlistenFn = fn;
        }
      })
      .catch((err) => {
        console.error("[tauri-transport] failed to register notebook:frame listener:", err);
      });

    const unlisten = () => {
      cancelled = true;
      if (unlistenFn) {
        unlistenFn();
        unlistenFn = null;
      }
    };

    this.unlisteners.push(unlisten);
    return unlisten;
  }

  async sendRequest(request: unknown, options?: NotebookRequestOptions): Promise<unknown> {
    const req = request as NotebookRequest;
    const id = crypto.randomUUID();

    // Wire shape matches Rust's `NotebookRequestEnvelope` with
    // `#[serde(tag = "action")]` on the flattened `NotebookRequest`:
    //   { id: "...", action: "execute_cell", cell_id: "..." }
    // TS uses `type:` as the discriminator internally, so translate on
    // the way out.
    const { type, ...rest } = req as { type: string } & Record<string, unknown>;
    const envelope = {
      id,
      ...(options?.required_heads?.length ? { required_heads: options.required_heads } : {}),
      action: type,
      ...rest,
    };
    const payload = new TextEncoder().encode(JSON.stringify(envelope));

    const timeoutMs = requestTimeoutMs(req);

    return new Promise<NotebookResponse>((resolve, reject) => {
      const timer = setTimeout(() => {
        if (this.pending.delete(id)) {
          reject(new Error(`Request timeout after ${timeoutMs}ms: ${type}`));
        }
      }, timeoutMs);

      this.pending.set(id, { resolve, reject, timer });

      // Fire the 0x01 frame. If `sendFrame` itself fails (rare — only if
      // the Tauri relay rejects the frame type), fail fast instead of
      // waiting out the timeout.
      void this.sendFrame(FRAME_TYPE_REQUEST, payload).catch((err) => {
        if (this.pending.delete(id)) {
          clearTimeout(timer);
          reject(err instanceof Error ? err : new Error(String(err)));
        }
      });
    });
  }

  disconnect(): void {
    this._connected = false;
    for (const unlisten of this.unlisteners) {
      unlisten();
    }
    this.unlisteners = [];
    // Reject any still-pending requests so callers don't hang.
    for (const [id, entry] of this.pending) {
      clearTimeout(entry.timer);
      entry.reject(new Error(`Transport disconnected (request ${id})`));
    }
    this.pending.clear();
  }

  /**
   * Attach a permanent tap on `notebook:frame` that intercepts `0x02`
   * response frames and dispatches them to the pending map.
   *
   * Non-response frames are ignored here — user-registered `onFrame`
   * callbacks (e.g., the SyncEngine) see every frame independently.
   * Tauri webview events fan out to all listeners, so the tap doesn't
   * steal frames from other subscribers.
   */
  private attachResponseTap(): void {
    const webview = getCurrentWebview();
    let unlistenFn: (() => void) | null = null;
    let cancelled = false;

    const unlistenPromise = webview.listen<number[]>("notebook:frame", (event) => {
      try {
        this.dispatchResponseFrame(event.payload);
      } catch (err) {
        console.error("[tauri-transport] response tap failed:", err);
      }
    });

    unlistenPromise
      .then((fn) => {
        if (cancelled) {
          fn();
        } else {
          unlistenFn = fn;
        }
      })
      .catch((err) => {
        console.error("[tauri-transport] failed to attach response tap:", err);
      });

    this.unlisteners.push(() => {
      cancelled = true;
      if (unlistenFn) {
        unlistenFn();
        unlistenFn = null;
      }
    });
  }

  private dispatchResponseFrame(bytes: number[] | Uint8Array): void {
    const first = (bytes as { [i: number]: number })[0];
    if (first !== FRAME_TYPE_RESPONSE) return;

    const view =
      bytes instanceof Uint8Array ? bytes.subarray(1) : new Uint8Array(Array.from(bytes).slice(1));
    const json = new TextDecoder().decode(view);

    let envelope: { id?: string } & Record<string, unknown>;
    try {
      envelope = JSON.parse(json);
    } catch (err) {
      console.error("[tauri-transport] malformed response envelope:", err);
      return;
    }

    const id = envelope.id;
    if (typeof id !== "string") return;

    const entry = this.pending.get(id);
    if (!entry) return;

    this.pending.delete(id);
    clearTimeout(entry.timer);

    // Strip the envelope id from the returned response so callers see a
    // clean `NotebookResponse`.
    const { id: _id, ...response } = envelope;
    entry.resolve(response as NotebookResponse);
  }
}
