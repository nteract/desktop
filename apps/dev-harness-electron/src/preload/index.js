"use strict";

// Dev-only Electron preload. Exposes `window.electronAPI` to the renderer
// via `contextBridge`. The renderer has `contextIsolation: true` and
// `nodeIntegration: false` — this preload is the only surface between
// renderer JS and Node-world IPC.

const { contextBridge, ipcRenderer } = require("electron");

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
  // Identifier the renderer can check to detect it's inside the dev harness.
  kind: "dev-harness-electron",

  sendFrame(type, payload) {
    // `payload` is a Uint8Array in the renderer; it survives the contextBridge
    // copy as a typed array. The main process accepts either Uint8Array or
    // plain number[].
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
});
