/**
 * WebSocketTransport — connects a SyncEngine to the daemon via the
 * live-viewer frame relay server.
 *
 * The relay server gives us a WebSocket that pipes raw typed frames
 * (frame_type byte + payload) bidirectionally to the daemon's RelayHandle.
 * This transport just wraps that WebSocket in the NotebookTransport interface.
 */

import type { NotebookTransport, FrameListener } from "runtimed/src/transport";
import { FrameType } from "runtimed/src/transport";

export interface WebSocketTransportOptions {
  /** WebSocket URL to the relay server (e.g. ws://host:8743/ws/join?id=...) */
  url: string;
  /** Called when the connection is established. */
  onOpen?: () => void;
  /** Called when the connection drops (includes close code). */
  onClose?: (code: number) => void;
  /** Called on WebSocket error. */
  onError?: (event: Event) => void;
}

export class WebSocketTransport implements NotebookTransport {
  private ws: WebSocket | null = null;
  private subscribers = new Set<FrameListener>();
  private _connected = false;
  private pendingRequests = new Map<
    string,
    { resolve: (value: unknown) => void; reject: (reason: unknown) => void }
  >();
  private requestIdCounter = 0;

  constructor(private readonly opts: WebSocketTransportOptions) {
    this.connect();
  }

  get connected(): boolean {
    return this._connected;
  }

  async sendFrame(frameType: number, payload: Uint8Array): Promise<void> {
    if (!this._connected || !this.ws) {
      throw new Error("WebSocketTransport: not connected");
    }
    const frame = new Uint8Array(1 + payload.length);
    frame[0] = frameType;
    frame.set(payload, 1);
    this.ws.send(frame);
  }

  onFrame(callback: FrameListener): () => void {
    this.subscribers.add(callback);
    return () => {
      this.subscribers.delete(callback);
    };
  }

  async sendRequest(request: unknown): Promise<unknown> {
    if (!this._connected || !this.ws) {
      throw new Error("WebSocketTransport: not connected");
    }
    const id = String(++this.requestIdCounter);
    const envelope = { id, request };
    const json = JSON.stringify(envelope);
    const bytes = new TextEncoder().encode(json);
    const frame = new Uint8Array(1 + bytes.length);
    frame[0] = FrameType.REQUEST;
    frame.set(bytes, 1);
    this.ws.send(frame);

    return new Promise((resolve, reject) => {
      this.pendingRequests.set(id, { resolve, reject });
    });
  }

  disconnect(): void {
    this._connected = false;
    this.subscribers.clear();
    for (const [, { reject }] of this.pendingRequests) {
      reject(new Error("WebSocketTransport: disconnected"));
    }
    this.pendingRequests.clear();
    if (this.ws) {
      this.ws.close();
      this.ws = null;
    }
  }

  private connect(): void {
    const ws = new WebSocket(this.opts.url);
    ws.binaryType = "arraybuffer";
    this.ws = ws;

    ws.onopen = () => {
      this._connected = true;
      this.opts.onOpen?.();
    };

    ws.onmessage = (event: MessageEvent) => {
      const data = new Uint8Array(event.data as ArrayBuffer);
      if (data.length === 0) return;

      const frameType = data[0];

      // Handle response frames (correlation ID resolution)
      if (frameType === FrameType.RESPONSE) {
        try {
          const json = new TextDecoder().decode(data.slice(1));
          const envelope = JSON.parse(json) as { id?: string; response?: unknown };
          if (envelope.id && this.pendingRequests.has(envelope.id)) {
            const { resolve } = this.pendingRequests.get(envelope.id)!;
            this.pendingRequests.delete(envelope.id);
            resolve(envelope.response);
            return;
          }
        } catch {
          // Fall through to deliver as raw frame
        }
      }

      // Deliver as raw typed frame (number[]) to match the FrameListener contract
      const frame = Array.from(data);
      for (const cb of this.subscribers) {
        try {
          cb(frame);
        } catch (err) {
          console.error("[WebSocketTransport] subscriber error:", err);
        }
      }
    };

    ws.onclose = (event: CloseEvent) => {
      this._connected = false;
      this.opts.onClose?.(event.code);
    };

    ws.onerror = (event: Event) => {
      this.opts.onError?.(event);
    };
  }
}
