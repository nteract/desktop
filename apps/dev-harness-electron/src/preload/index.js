"use strict";

// Dev-only Electron preload.
//
// Exposes two surfaces on the renderer's `window` via `contextBridge`:
//
//   1. `window.electronAPI` — the preferred surface for harness-aware code
//      (apps/notebook/src/lib/electron-transport.ts). Used by ElectronTransport
//      for sendFrame / onFrame / sendRequest.
//
//   2. `window.__TAURI_INTERNALS__` (+ friends) — a shim so `@tauri-apps/api`
//      calls don't crash when the notebook frontend loads in Electron.
//      `getCurrentWindow()` reads metadata.currentWindow.label; `listen()`
//      goes through invoke; all of this would otherwise throw because
//      Electron is not Tauri. The shim no-ops Tauri-chrome commands and
//      routes known `*_via_daemon` commands to the harness's daemon
//      connection via ipcRenderer.
//
// `contextIsolation: true` means this preload runs in a separate JS world
// from the renderer. `contextBridge.exposeInMainWorld` is the only way to
// share state — values get deep-cloned and functions become callable proxies.

const { contextBridge, ipcRenderer } = require("electron");

// ── electronAPI ────────────────────────────────────────────────────────────

const frameListeners = new Set();

ipcRenderer.on("notebook-frame", (_event, bytes) => {
  for (const cb of frameListeners) {
    try {
      cb(bytes);
    } catch (err) {
      console.error("[dev-harness preload] frame listener threw:", err);
    }
  }
});

ipcRenderer.on("daemon-disconnected", () => {
  for (const cb of frameListeners) {
    try {
      cb({ __daemonDisconnected: true });
    } catch {}
  }
});

contextBridge.exposeInMainWorld("electronAPI", {
  kind: "dev-harness-electron",

  sendFrame(type, payload) {
    return ipcRenderer.invoke("send-frame", { type, payload });
  },

  onFrame(callback) {
    frameListeners.add(callback);
    return () => frameListeners.delete(callback);
  },

  sendRequest(request) {
    return ipcRenderer.invoke("send-request", request);
  },

  info() {
    return ipcRenderer.invoke("dev-harness:info");
  },

  // Signal to main that the frame listener is attached and it's safe to
  // replay buffered frames. Mirrors Tauri's `notify_sync_ready`.
  signalReady() {
    return ipcRenderer.invoke("dev-harness:ready");
  },
});

// ── Tauri shim ─────────────────────────────────────────────────────────────
//
// Minimal surface to keep `@tauri-apps/api` imports from throwing. The shim
// lives entirely in-preload except for `invoke`, which IPCs to the main
// process so daemon-request commands can be routed through the Unix socket.

let nextCallbackId = 1;
const callbacks = new Map();
const tauriEventListeners = new Map(); // eventName → Map<id, unlisten>

contextBridge.exposeInMainWorld("__TAURI_INTERNALS__", {
  // `invoke` is the core IPC path. The main process decides which commands
  // are daemon requests (routed through the existing `send-request` handler)
  // and which are Tauri-chrome no-ops.
  invoke(cmd, args /*, options */) {
    return ipcRenderer.invoke("tauri-shim:invoke", cmd, args);
  },

  // `transformCallback` registers a callback and returns a numeric id.
  // Tauri uses this for event plumbing and async streams. The harness
  // doesn't drive these paths (window-level events, update events, etc.),
  // so a pure bookkeeping impl is sufficient.
  transformCallback(callback /*, once */) {
    const id = nextCallbackId++;
    callbacks.set(id, callback);
    return id;
  },

  unregisterCallback(id) {
    callbacks.delete(id);
  },

  convertFileSrc(filePath, protocol = "asset") {
    // In Tauri this returns `<protocol>://localhost/<urlencoded path>`. For
    // the harness we don't serve files by convention; return unchanged.
    void protocol;
    return filePath;
  },

  // Consumed by `getCurrentWindow` / `getCurrentWebview`. The label is cosmetic.
  metadata: {
    currentWindow: { label: "dev-harness" },
    currentWebview: { label: "dev-harness" },
  },
});

// Tauri's event system expects this object. `unregisterListener(event, id)`
// is the only method called frequently. No-op: the harness routes daemon
// frames via electronAPI.onFrame, not via Tauri events.
contextBridge.exposeInMainWorld("__TAURI_EVENT_PLUGIN_INTERNALS__", {
  unregisterListener(event, eventId) {
    const listeners = tauriEventListeners.get(event);
    if (listeners) listeners.delete(eventId);
  },
});

// `isTauri()` reads `(globalThis || window).isTauri`. We return true so code
// that feature-gates on it still runs — the shim handles the downstream calls.
contextBridge.exposeInMainWorld("isTauri", true);

// Opt the notebook UI out of iframe isolation for widget-view+json outputs
// when running under this harness. The iframe's postMessage bootstrap hasn't
// been fully shimmed through to Electron, so built-in widgets never mount.
// Rendering `<WidgetView />` directly in the parent DOM (via the MediaProvider
// renderer in apps/notebook/src/App.tsx) sidesteps the issue entirely and
// lets Playwright drive sliders natively. Scope: dev harness only.
contextBridge.exposeInMainWorld("__NTERACT_DEV_HARNESS_INLINE_WIDGETS__", true);
