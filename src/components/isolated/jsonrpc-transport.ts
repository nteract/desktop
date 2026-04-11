/**
 * Lightweight JSON-RPC 2.0 transport over postMessage.
 *
 * Provides a simple `notify`/`request`/`onNotification`/`onRequest` API
 * for bidirectional communication between host and iframe. Extracts
 * ArrayBuffer instances as transferables for zero-copy transfer of
 * widget buffers (e.g., Apache Arrow tables).
 *
 * Silently ignores non-JSON-RPC messages (legacy `{ type, payload }` format)
 * so both formats can coexist during migration.
 */

// ── Types ───────────────────────────────────────────────────────────

interface JsonRpcNotification {
  jsonrpc: "2.0";
  method: string;
  params?: unknown;
}

interface JsonRpcRequest {
  jsonrpc: "2.0";
  id: number;
  method: string;
  params?: unknown;
}

interface JsonRpcResponse {
  jsonrpc: "2.0";
  id: number;
  result?: unknown;
  error?: { code: number; message: string; data?: unknown };
}

type JsonRpcMessage = JsonRpcNotification | JsonRpcRequest | JsonRpcResponse;

type NotificationHandler = (params: unknown) => void;
type RequestHandler = (params: unknown) => unknown | Promise<unknown>;

// ── Helpers ─────────────────────────────────────────────────────────

/**
 * Recursively collect all ArrayBuffer instances from a value.
 * Uses a visited set to handle cyclic references safely.
 */
function collectArrayBuffers(
  value: unknown,
  buffers: ArrayBuffer[],
  visited: Set<object> = new Set(),
): void {
  if (value instanceof ArrayBuffer) {
    buffers.push(value);
    return;
  }
  if (ArrayBuffer.isView(value) && value.buffer instanceof ArrayBuffer) {
    buffers.push(value.buffer);
    return;
  }
  if (typeof value !== "object" || value === null) return;
  if (visited.has(value)) return;
  visited.add(value);
  if (Array.isArray(value)) {
    for (const item of value) {
      collectArrayBuffers(item, buffers, visited);
    }
    return;
  }
  for (const v of Object.values(value)) {
    collectArrayBuffers(v, buffers, visited);
  }
}

function isJsonRpcMessage(data: unknown): data is JsonRpcMessage {
  return (
    typeof data === "object" && data !== null && (data as { jsonrpc?: unknown }).jsonrpc === "2.0"
  );
}

function isRequest(msg: JsonRpcMessage): msg is JsonRpcRequest {
  return "id" in msg && "method" in msg;
}

function isResponse(msg: JsonRpcMessage): msg is JsonRpcResponse {
  return "id" in msg && !("method" in msg);
}

// ── Transport ───────────────────────────────────────────────────────

export class JsonRpcTransport {
  private target: Window;
  private source: MessageEventSource;
  private listener: ((event: MessageEvent) => void) | null = null;
  private nextId = 1;

  private notificationHandlers = new Map<string, NotificationHandler>();
  private requestHandlers = new Map<string, RequestHandler>();
  private pendingRequests = new Map<
    number,
    { resolve: (value: unknown) => void; reject: (error: Error) => void }
  >();

  constructor(target: Window, source: MessageEventSource) {
    this.target = target;
    this.source = source;
  }

  // ── Sending ─────────────────────────────────────────────────────

  /**
   * Send a JSON-RPC notification (fire-and-forget, no response expected).
   */
  notify(method: string, params?: unknown): void {
    const msg: JsonRpcNotification = { jsonrpc: "2.0", method, params };
    this.send(msg);
  }

  /**
   * Send a JSON-RPC request and wait for a response.
   */
  request(method: string, params?: unknown): Promise<unknown> {
    const id = this.nextId++;
    const msg: JsonRpcRequest = { jsonrpc: "2.0", id, method, params };

    return new Promise((resolve, reject) => {
      this.pendingRequests.set(id, { resolve, reject });
      this.send(msg);
    });
  }

  // ── Receiving ───────────────────────────────────────────────────

  /**
   * Register a handler for incoming notifications of a given method.
   */
  onNotification(method: string, handler: NotificationHandler): void {
    this.notificationHandlers.set(method, handler);
  }

  /**
   * Register a handler for incoming requests of a given method.
   * The handler's return value (or resolved promise) is sent back as the response.
   */
  onRequest(method: string, handler: RequestHandler): void {
    this.requestHandlers.set(method, handler);
  }

  // ── Lifecycle ───────────────────────────────────────────────────

  /**
   * Start listening for postMessage events.
   */
  start(): void {
    if (this.listener) return;

    this.listener = (event: MessageEvent) => {
      // Validate source for security
      if (event.source !== this.source) return;

      const data = event.data;
      if (!isJsonRpcMessage(data)) return; // Silently ignore non-JSON-RPC

      if (isResponse(data)) {
        // Response to a pending request
        const pending = this.pendingRequests.get(data.id);
        if (pending) {
          this.pendingRequests.delete(data.id);
          if (data.error) {
            pending.reject(new Error(data.error.message));
          } else {
            pending.resolve(data.result);
          }
        }
      } else if (isRequest(data)) {
        // Incoming request — dispatch to handler and send response
        const handler = this.requestHandlers.get(data.method);
        if (handler) {
          let result: unknown;
          try {
            result = handler(data.params);
          } catch (err) {
            const response: JsonRpcResponse = {
              jsonrpc: "2.0",
              id: data.id,
              error: {
                code: -32000,
                message: err instanceof Error ? err.message : String(err),
              },
            };
            this.target.postMessage(response, "*");
            return;
          }
          Promise.resolve(result).then(
            (result) => {
              const response: JsonRpcResponse = {
                jsonrpc: "2.0",
                id: data.id,
                result,
              };
              this.target.postMessage(response, "*");
            },
            (err) => {
              const response: JsonRpcResponse = {
                jsonrpc: "2.0",
                id: data.id,
                error: {
                  code: -32000,
                  message: err instanceof Error ? err.message : String(err),
                },
              };
              this.target.postMessage(response, "*");
            },
          );
        } else {
          // Method not found — send error response
          const response: JsonRpcResponse = {
            jsonrpc: "2.0",
            id: data.id,
            error: {
              code: -32601,
              message: `Method not found: ${data.method}`,
            },
          };
          this.target.postMessage(response, "*");
        }
      } else {
        // Notification — dispatch to handler
        const handler = this.notificationHandlers.get(data.method);
        if (handler) {
          handler(data.params);
        }
        // Unknown notifications are silently ignored
      }
    };

    window.addEventListener("message", this.listener);
  }

  /**
   * Stop listening and reject all pending requests.
   */
  stop(): void {
    if (this.listener) {
      window.removeEventListener("message", this.listener);
      this.listener = null;
    }
    // Reject pending requests
    for (const [, { reject }] of this.pendingRequests) {
      reject(new Error("Transport stopped"));
    }
    this.pendingRequests.clear();
  }

  // ── Internal ────────────────────────────────────────────────────

  private send(msg: JsonRpcMessage): void {
    // Collect ArrayBuffers for zero-copy transfer
    const transferables: ArrayBuffer[] = [];
    collectArrayBuffers(msg, transferables);
    const unique = [...new Set(transferables)];

    if (unique.length > 0) {
      this.target.postMessage(msg, "*", unique);
    } else {
      this.target.postMessage(msg, "*");
    }
  }
}
