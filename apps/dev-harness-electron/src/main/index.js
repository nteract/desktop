"use strict";

// Dev-only Electron harness main process.
//
// Opens the runtimed daemon's Unix socket from Node, relays bytes to a renderer
// that loads the existing notebook frontend from Vite dev (port 5174 by default,
// overridable via RUNTIMED_VITE_PORT). Mirrors what crates/notebook/src/lib.rs
// does for Tauri, just in Node.
//
// Never shipped. Launched via `pnpm --filter @nteract/dev-harness-electron dev`
// or a Playwright fixture. No network listener is opened.

const { app, BrowserWindow, ipcMain } = require("electron");
const net = require("node:net");
const path = require("node:path");
const { execFileSync } = require("node:child_process");

// ---------- Wire protocol constants (mirrors crates/notebook-protocol) ----------

const MAGIC = Buffer.from([0xc0, 0xde, 0x01, 0xac]);
const PROTOCOL_VERSION = 2;
const PROTOCOL_V2 = "v2";
const PREAMBLE = Buffer.concat([MAGIC, Buffer.from([PROTOCOL_VERSION])]);

const FRAME_TYPE = {
  AUTOMERGE_SYNC: 0x00,
  REQUEST: 0x01,
  RESPONSE: 0x02,
  BROADCAST: 0x03,
  PRESENCE: 0x04,
  RUNTIME_STATE_SYNC: 0x05,
  POOL_STATE_SYNC: 0x06,
};

// Per connection.rs: outer ceiling 100 MiB, Presence 1 MiB.
// Any longer length prefix from a desynced stream is dropped before we
// try to allocate for it.
const MAX_FRAME_SIZE = 100 * 1024 * 1024;
const MAX_PRESENCE_FRAME_SIZE = 1024 * 1024;

function maxPayloadSizeForFrameType(typeByte) {
  if (typeByte === FRAME_TYPE.PRESENCE) return MAX_PRESENCE_FRAME_SIZE;
  return MAX_FRAME_SIZE;
}

const OUTBOUND_FRAME_ALLOWED = new Set([
  FRAME_TYPE.AUTOMERGE_SYNC,
  FRAME_TYPE.PRESENCE,
  FRAME_TYPE.RUNTIME_STATE_SYNC,
  FRAME_TYPE.POOL_STATE_SYNC,
]);

// ---------- CLI parsing ----------

function parseArgs(argv) {
  const out = {
    socket: process.env.RUNTIMED_SOCKET_PATH || null,
    notebookId: process.env.HARNESS_NOTEBOOK_ID || null,
    notebookPath: process.env.HARNESS_NOTEBOOK_PATH || null,
    workingDir: process.env.HARNESS_WORKING_DIR || process.cwd(),
    rendererUrl: null,
  };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--socket") out.socket = argv[++i];
    else if (a === "--notebook-id") out.notebookId = argv[++i];
    else if (a === "--notebook-path") out.notebookPath = argv[++i];
    else if (a === "--working-dir") out.workingDir = argv[++i];
    else if (a === "--renderer-url") out.rendererUrl = argv[++i];
  }
  return out;
}

let cachedDaemonStatus = null;

function fetchDaemonStatus() {
  if (cachedDaemonStatus) return cachedDaemonStatus;
  const runtBin = process.env.RUNTIMED_CLI_BIN || "runt";
  try {
    const stdout = execFileSync(runtBin, ["daemon", "status", "--json"], {
      encoding: "utf8",
      timeout: 5000,
      env: process.env,
    });
    cachedDaemonStatus = JSON.parse(stdout);
    return cachedDaemonStatus;
  } catch (err) {
    console.error("[dev-harness] failed to fetch daemon status:", err.message);
    return null;
  }
}

function discoverSocketPath() {
  if (process.env.RUNTIMED_SOCKET_PATH) return process.env.RUNTIMED_SOCKET_PATH;
  const status = fetchDaemonStatus();
  return (status && status.socket_path) || null;
}

function discoverBlobPort() {
  const status = fetchDaemonStatus();
  return (status && status.daemon_info && status.daemon_info.blob_port) || null;
}

// ---------- Frame codec ----------

function encodeLengthPrefixed(body) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(body.length, 0);
  return Buffer.concat([len, body]);
}

function encodeTypedFrame(typeByte, payload) {
  const body = Buffer.concat([Buffer.from([typeByte]), Buffer.from(payload)]);
  return encodeLengthPrefixed(body);
}

// Accumulates raw bytes; yields one length-prefixed frame body at a time.
// The caller interprets the body — for the first frame it's raw JSON (handshake
// response); thereafter the body starts with a 1-byte type discriminator.
class FrameBuffer {
  constructor() {
    this.buf = Buffer.alloc(0);
  }
  push(chunk) {
    this.buf = this.buf.length === 0 ? chunk : Buffer.concat([this.buf, chunk]);
  }
  // Returns { body } on success, { err } on desync, null when waiting for more bytes.
  tryTakeOne() {
    if (this.buf.length < 4) return null;
    const len = this.buf.readUInt32BE(0);
    if (len === 0) {
      return { err: new Error("empty frame") };
    }
    if (len > MAX_FRAME_SIZE) {
      return {
        err: new Error(`frame too large: ${len} bytes (max ${MAX_FRAME_SIZE})`),
      };
    }
    if (this.buf.length < 4 + len) return null;
    const body = this.buf.subarray(4, 4 + len);
    // Per-type ceiling: inspect the 1-byte discriminator when present.
    if (body.length > 0) {
      const typeByte = body[0];
      const bodyLen = body.length - 1;
      const cap = maxPayloadSizeForFrameType(typeByte);
      if (bodyLen > cap) {
        this.buf = this.buf.subarray(4 + len);
        return {
          err: new Error(
            `frame too large for type 0x${typeByte.toString(16)}: ${bodyLen} bytes (max ${cap})`,
          ),
        };
      }
    }
    this.buf = this.buf.subarray(4 + len);
    return { body };
  }
}

// ---------- Daemon connection ----------

class DaemonConnection {
  constructor(socketPath, onTypedFrame, onClose) {
    this.socketPath = socketPath;
    this.onTypedFrame = onTypedFrame; // ({ typeByte, payload }) => void
    this.onClose = onClose;
    this.socket = null;
    this.buf = new FrameBuffer();
    this.handshakeResolved = false;
    this.handshakeResolve = null;
    this.handshakeReject = null;
    this.pendingResponse = null;
    this.requestQueue = [];
    this.processingRequest = false;
    this.connected = false;
  }

  connect(handshake) {
    return new Promise((resolve, reject) => {
      this.handshakeResolve = resolve;
      this.handshakeReject = reject;
      this.socket = net.createConnection(this.socketPath, () => {
        this.connected = true;
        try {
          this.socket.write(PREAMBLE);
          this.socket.write(encodeLengthPrefixed(Buffer.from(JSON.stringify(handshake))));
        } catch (e) {
          reject(e);
        }
      });
      this.socket.on("error", (err) => {
        if (!this.handshakeResolved) reject(err);
        else this._handleClose(err);
      });
      this.socket.on("close", () => this._handleClose(null));
      this.socket.on("data", (chunk) => this._handleData(chunk));
    });
  }

  _handleData(chunk) {
    this.buf.push(chunk);
    while (true) {
      const next = this.buf.tryTakeOne();
      if (!next) return;
      if (next.err) {
        console.error("[dev-harness] frame decode error:", next.err.message);
        this._handleClose(next.err);
        try {
          this.socket.destroy(next.err);
        } catch {}
        return;
      }
      const body = next.body;

      if (!this.handshakeResolved) {
        this.handshakeResolved = true;
        try {
          const info = JSON.parse(body.toString("utf8"));
          this.handshakeResolve(info);
        } catch (e) {
          this.handshakeReject(new Error(`handshake parse: ${e.message}`));
        }
        continue;
      }

      // Post-handshake: body = [typeByte, ...payload]
      const typeByte = body.length > 0 ? body[0] : 0;
      const payload = body.subarray(1);

      if (typeByte === FRAME_TYPE.RESPONSE) {
        if (this.pendingResponse) {
          const { resolve } = this.pendingResponse;
          this.pendingResponse = null;
          try {
            resolve(JSON.parse(payload.toString("utf8")));
          } catch (e) {
            resolve({ result: "error", error: `response parse: ${e.message}` });
          }
          this._drainRequestQueue();
        } else {
          // Unsolicited Response — e.g., stale frame from a request that
          // already timed out. Rust's relay drops these (relay_task.rs
          // `pipe_frame` filters type 0x02). Log and drop for parity.
          console.warn("[dev-harness] unsolicited Response frame dropped (no pending request)");
        }
      } else {
        this.onTypedFrame({ typeByte, payload });
      }
    }
  }

  _handleClose(err) {
    this.connected = false;
    if (this.pendingResponse) {
      const { reject } = this.pendingResponse;
      this.pendingResponse = null;
      reject(err || new Error("daemon connection closed"));
    }
    for (const { reject } of this.requestQueue) {
      reject(err || new Error("daemon connection closed"));
    }
    this.requestQueue = [];
    if (this.onClose) this.onClose(err);
  }

  sendFrame(typeByte, payloadBytes) {
    if (!this.connected) throw new Error("daemon not connected");
    if (!OUTBOUND_FRAME_ALLOWED.has(typeByte)) {
      throw new Error(`outbound frame type 0x${typeByte.toString(16)} is not allowed`);
    }
    this.socket.write(encodeTypedFrame(typeByte, payloadBytes));
  }

  sendRequest(request) {
    return new Promise((resolve, reject) => {
      this.requestQueue.push({ request, resolve, reject });
      this._drainRequestQueue();
    });
  }

  _drainRequestQueue() {
    if (this.processingRequest) return;
    if (this.requestQueue.length === 0) return;
    if (!this.connected) return;
    const next = this.requestQueue.shift();
    this.processingRequest = true;
    this.pendingResponse = {
      resolve: (value) => {
        this.processingRequest = false;
        next.resolve(value);
      },
      reject: (err) => {
        this.processingRequest = false;
        next.reject(err);
      },
    };
    const payload = Buffer.from(JSON.stringify(next.request));
    this.socket.write(encodeTypedFrame(FRAME_TYPE.REQUEST, payload));
  }
}

// ---------- Electron wiring ----------

let mainWindow = null;
let daemon = null;

function buildHandshake(cli) {
  if (cli.notebookPath) {
    return { channel: "open_notebook", path: cli.notebookPath };
  }
  if (cli.notebookId) {
    return {
      channel: "notebook_sync",
      notebook_id: cli.notebookId,
      protocol: PROTOCOL_V2,
      working_dir: cli.workingDir,
    };
  }
  return {
    channel: "create_notebook",
    runtime: "python",
    working_dir: cli.workingDir,
  };
}

// `--renderer-url` is a convenience for pointing the harness at a different
// dev port; anything non-loopback would be a confused-deputy: the preload
// bridges full daemon access to whatever origin loads. Restrict to localhost.
function isLocalhostUrl(url) {
  try {
    const u = new URL(url);
    if (u.protocol !== "http:" && u.protocol !== "https:" && u.protocol !== "file:") {
      return false;
    }
    return u.hostname === "localhost" || u.hostname === "127.0.0.1" || u.hostname === "[::1]";
  } catch {
    return false;
  }
}

async function createWindow(cli) {
  const vitePort = process.env.RUNTIMED_VITE_PORT || process.env.CONDUCTOR_PORT || "5174";
  const rendererUrl = cli.rendererUrl || `http://localhost:${vitePort}/`;
  if (!isLocalhostUrl(rendererUrl)) {
    throw new Error(
      `--renderer-url must point to localhost (got ${rendererUrl}). The harness exposes daemon IPC via preload; loading a remote origin would hand it daemon access.`,
    );
  }

  mainWindow = new BrowserWindow({
    width: 1400,
    height: 900,
    title: "nteract (dev-harness)",
    webPreferences: {
      preload: path.join(__dirname, "..", "preload", "index.js"),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  });

  // Block navigation and window.open away from the permitted origin. Without
  // this the renderer could load a remote origin in-place and inherit the
  // preload's electronAPI surface (see codex review, finding #4).
  mainWindow.webContents.on("will-navigate", (event, url) => {
    if (!isLocalhostUrl(url)) {
      console.warn(`[dev-harness] blocked navigation to non-localhost: ${url}`);
      event.preventDefault();
    }
  });
  mainWindow.webContents.setWindowOpenHandler(({ url }) => {
    if (isLocalhostUrl(url)) return { action: "allow" };
    console.warn(`[dev-harness] blocked window.open to non-localhost: ${url}`);
    return { action: "deny" };
  });

  mainWindow.on("closed", () => {
    mainWindow = null;
  });

  await mainWindow.loadURL(rendererUrl);
}

// Until the renderer calls `dev-harness:ready`, inbound frames are buffered
// here and replayed once the window is listening. Mirrors Tauri's
// `notify_sync_ready` gate (crates/notebook/src/lib.rs notifyReady flag)
// which prevents frame loss during WASM init on first connect.
let rendererReady = false;
const pendingFrames = [];

function sendFrameToRenderer(typeByte, payload) {
  const buf = Buffer.concat([Buffer.from([typeByte]), payload]);
  const bytes = Array.from(buf);
  if (!rendererReady || !mainWindow || mainWindow.isDestroyed()) {
    pendingFrames.push(bytes);
    return;
  }
  mainWindow.webContents.send("notebook-frame", bytes);
}

function drainPendingFramesToRenderer() {
  if (!mainWindow || mainWindow.isDestroyed()) return;
  while (pendingFrames.length > 0) {
    mainWindow.webContents.send("notebook-frame", pendingFrames.shift());
  }
}

app.whenReady().then(async () => {
  const cli = parseArgs(process.argv.slice(2));
  if (!cli.socket) cli.socket = discoverSocketPath();
  if (!cli.socket) {
    console.error(
      "[dev-harness] could not resolve daemon socket path — set RUNTIMED_SOCKET_PATH or start the dev daemon.",
    );
    app.exit(1);
    return;
  }

  console.log(`[dev-harness] connecting to daemon socket: ${cli.socket}`);
  console.log("[dev-harness] handshake target:", {
    notebook_id: cli.notebookId,
    notebook_path: cli.notebookPath,
    working_dir: cli.workingDir,
  });

  daemon = new DaemonConnection(
    cli.socket,
    ({ typeByte, payload }) => sendFrameToRenderer(typeByte, payload),
    (err) => {
      console.error("[dev-harness] daemon disconnected:", err && err.message);
      if (mainWindow && !mainWindow.isDestroyed()) {
        mainWindow.webContents.send("daemon-disconnected");
      }
    },
  );

  const handshake = buildHandshake(cli);
  let connectionInfo;
  try {
    connectionInfo = await daemon.connect(handshake);
  } catch (err) {
    console.error("[dev-harness] handshake failed:", err.message);
    app.exit(1);
    return;
  }
  console.log("[dev-harness] handshake ok:", {
    notebook_id: connectionInfo && connectionInfo.notebook_id,
    cell_count: connectionInfo && connectionInfo.cell_count,
  });

  ipcMain.handle("dev-harness:info", () => ({
    notebookId: connectionInfo && connectionInfo.notebook_id,
    cellCount: connectionInfo && connectionInfo.cell_count,
  }));

  // Sync IPC so the preload (sandbox:true) can read env-driven flags
  // without process.env access.
  ipcMain.on("harness:get-config", (event) => {
    event.returnValue = {
      inlineWidgets: process.env.HARNESS_INLINE_WIDGETS !== "0",
    };
  });

  // The renderer calls this once ElectronTransport has its onFrame
  // listener attached. Until then, inbound frames are buffered in
  // `pendingFrames` rather than sent into the void.
  ipcMain.handle("dev-harness:ready", () => {
    rendererReady = true;
    drainPendingFramesToRenderer();
    return { ok: true };
  });

  ipcMain.handle("send-frame", (_event, { type, payload }) => {
    try {
      const bytes = payload instanceof Uint8Array ? payload : Buffer.from(payload);
      daemon.sendFrame(type, bytes);
      return { ok: true };
    } catch (err) {
      return { ok: false, error: err.message };
    }
  });

  ipcMain.handle("send-request", async (_event, request) => {
    try {
      // NotebookClient on the TS side builds `{ type: "launch_kernel", ... }`
      // (see packages/runtimed/src/request-types.ts), but the wire protocol
      // is serde-tagged with `action` (crates/notebook-protocol/src/
      // protocol.rs `#[serde(tag = "action")]`). Rename here so we don't
      // need to touch the transport-agnostic request types.
      const wireRequest = toWireRequest(request);
      return await daemon.sendRequest(wireRequest);
    } catch (err) {
      return { result: "error", error: err.message };
    }
  });

  // ── Tauri shim router ─────────────────────────────────────────────────
  //
  // The notebook UI imports `@tauri-apps/api/core` in ~15 places. The preload
  // installs a `window.__TAURI_INTERNALS__` shim; its `invoke` IPCs here. We
  // map the daemon-request subset to NotebookRequest frames and no-op the
  // Tauri-chrome surface (auto-updater, trust prompts, window state).
  ipcMain.handle("tauri-shim:invoke", async (_event, cmd, args) => {
    const daemonReq = mapTauriCommandToNotebookRequest(cmd, args);
    if (daemonReq) {
      try {
        return await daemon.sendRequest(daemonReq);
      } catch (err) {
        throw new Error(`daemon request failed for ${cmd}: ${err.message}`);
      }
    }
    return tauriShimNoop(cmd, args);
  });

  await createWindow(cli);
});

// Translate the TS-side NotebookRequest shape (`{ type, ... }`) into the
// wire shape (`{ action, ... }`). Also accepts requests that already use
// `action` so callers from the Tauri-shim path pass through unchanged.
function toWireRequest(request) {
  if (!request || typeof request !== "object") return request;
  if ("action" in request) return request;
  if ("type" in request) {
    const { type, ...rest } = request;
    // NotebookRequest::InterruptExecution uses `interrupt_execution` on the
    // wire; the TS side uses `interrupt` for brevity.
    const action = type === "interrupt" ? "interrupt_execution" : type;
    return { action, ...rest };
  }
  return request;
}

// Translate Tauri command names to NotebookRequest payloads. The Rust side
// of the Tauri app has one-off Tauri commands that each build and send a
// NotebookRequest; the harness short-circuits them to avoid maintaining a
// separate router.
function mapTauriCommandToNotebookRequest(cmd, args) {
  switch (cmd) {
    case "launch_kernel_via_daemon":
      return {
        action: "launch_kernel",
        kernel_type: args.kernelType,
        env_source: args.envSource,
        notebook_path: args.notebookPath ?? null,
      };
    case "execute_cell_via_daemon":
      return { action: "execute_cell", cell_id: args.cellId };
    case "clear_outputs_via_daemon":
      return { action: "clear_outputs", cell_id: args.cellId };
    case "interrupt_via_daemon":
      return { action: "interrupt_execution" };
    case "shutdown_kernel_via_daemon":
      return { action: "shutdown_kernel" };
    case "sync_environment_via_daemon":
      return { action: "sync_environment" };
    case "run_all_cells_via_daemon":
      return { action: "run_all_cells" };
    case "send_comm_via_daemon":
      return { action: "send_comm", message: args.message };
    default:
      return null;
  }
}

// Tauri-chrome commands handled with fixed default values (no daemon call).
// The handlers cover the subset the notebook UI calls at startup; everything
// else falls through to a silent no-op.
function tauriShimNoop(cmd, _args) {
  switch (cmd) {
    // Blob server discovery — real port from daemon status.
    case "get_blob_port": {
      const port = discoverBlobPort();
      if (port == null) throw new Error("Blob server not available");
      return port;
    }

    // Dev harness is implicitly trusted — we launched it locally. Shape
    // matches TrustInfo in apps/notebook/src/hooks/useTrust.ts so the hook's
    // spread-of-dependencies doesn't crash.
    case "verify_notebook_trust":
      return {
        status: "trusted",
        uv_dependencies: [],
        conda_dependencies: [],
        conda_channels: [],
      };

    case "check_typosquats":
      return [];

    // Settings live in a per-user Automerge doc on the daemon. Returning an
    // empty object makes the frontend fall back to built-in defaults.
    case "get_synced_settings":
      return {};

    // Optional UI chrome — return null / empty so banners hide themselves.
    case "get_git_info":
    case "get_daemon_info":
    case "detect_pyproject":
    case "detect_environment_yml":
    case "detect_pixi_toml":
    case "detect_deno_config":
      return null;

    case "get_username":
      return process.env.USER || process.env.USERNAME || "dev-harness";

    // App.tsx polls this 500ms after mount — returning undefined makes it
    // show "Runtime daemon not available". We connected at startup and
    // won't lose the connection mid-session.
    case "is_daemon_connected":
      return daemon?.connected === true;

    // Fire-and-forget frontend signals (relay gates, window-title sync, etc.).
    case "notify_sync_ready":
    case "reconnect_to_daemon":
    case "approve_notebook_trust":
    case "mark_notebook_clean":
    case "apply_path_changed":
    case "import_pyproject_dependencies":
      return undefined;

    default:
      // Log once-per-command so the harness surfaces unhandled Tauri surface area.
      if (!cmd.startsWith("plugin:")) {
        tauriShimNoop._logged = tauriShimNoop._logged || new Set();
        if (!tauriShimNoop._logged.has(cmd)) {
          tauriShimNoop._logged.add(cmd);
          console.warn(`[tauri-shim] no-op for unknown command: ${cmd}`);
        }
      }
      return undefined;
  }
}

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") app.quit();
});
