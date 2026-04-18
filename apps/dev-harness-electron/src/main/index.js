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

function discoverSocketPath() {
  if (process.env.RUNTIMED_SOCKET_PATH) return process.env.RUNTIMED_SOCKET_PATH;
  const runtBin = process.env.RUNTIMED_CLI_BIN || "runt";
  try {
    const stdout = execFileSync(runtBin, ["daemon", "status", "--json"], {
      encoding: "utf8",
      timeout: 5000,
      env: process.env,
    });
    const info = JSON.parse(stdout);
    if (info && typeof info.socket_path === "string") return info.socket_path;
  } catch (err) {
    console.error(
      "[dev-harness] failed to discover socket path via `runt daemon status`:",
      err.message,
    );
  }
  return null;
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
  tryTakeOne() {
    if (this.buf.length < 4) return null;
    const len = this.buf.readUInt32BE(0);
    if (this.buf.length < 4 + len) return null;
    const body = this.buf.subarray(4, 4 + len);
    this.buf = this.buf.subarray(4 + len);
    return body;
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
      const body = this.buf.tryTakeOne();
      if (!body) return;

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

      if (typeByte === FRAME_TYPE.RESPONSE && this.pendingResponse) {
        const { resolve } = this.pendingResponse;
        this.pendingResponse = null;
        try {
          resolve(JSON.parse(payload.toString("utf8")));
        } catch (e) {
          resolve({ result: "error", error: `response parse: ${e.message}` });
        }
        this._drainRequestQueue();
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
      working_dir: cli.workingDir,
    };
  }
  return {
    channel: "create_notebook",
    runtime: "python",
    working_dir: cli.workingDir,
  };
}

async function createWindow(cli) {
  const vitePort = process.env.RUNTIMED_VITE_PORT || process.env.CONDUCTOR_PORT || "5174";
  const rendererUrl = cli.rendererUrl || `http://localhost:${vitePort}/`;

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

  mainWindow.on("closed", () => {
    mainWindow = null;
  });

  await mainWindow.loadURL(rendererUrl);
}

function sendFrameToRenderer(typeByte, payload) {
  if (!mainWindow || mainWindow.isDestroyed()) return;
  const buf = Buffer.concat([Buffer.from([typeByte]), payload]);
  // Wire parity with Tauri's `notebook:frame` event, which delivers number[].
  mainWindow.webContents.send("notebook-frame", Array.from(buf));
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
      return await daemon.sendRequest(request);
    } catch (err) {
      return { result: "error", error: err.message };
    }
  });

  await createWindow(cli);
});

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") app.quit();
});
