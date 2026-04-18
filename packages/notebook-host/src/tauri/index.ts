/**
 * `createTauriHost()` — Tauri desktop app implementation of `NotebookHost`.
 *
 * Zero behavior change compared to the direct `invoke(…)` / `webview.listen(…)`
 * calls the frontend used previously. Every method here is a thin wrapper
 * around an existing Tauri command or plugin call, shaped to match the
 * `NotebookHost` interface so call sites stop importing `@tauri-apps/api`
 * directly.
 *
 * The transport is passed in rather than constructed here because the
 * `TauriTransport` class currently lives in `apps/notebook/src/lib/` and
 * hooks into the app's logger. A later PR will move it into this package
 * and tighten the import direction.
 */

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { NotebookTransport } from "runtimed";

import type {
  DaemonInfo,
  DaemonProgressPayload,
  DaemonReadyPayload,
  DaemonUnavailablePayload,
  GitInfo,
  HostBlobs,
  HostDaemon,
  HostDaemonEvents,
  HostNotebook,
  HostSystem,
  HostTrust,
  NotebookHost,
  TrustInfo,
  TyposquatWarning,
  Unlisten,
} from "../types";

export interface CreateTauriHostOptions {
  /** The `NotebookTransport` instance to expose at `host.transport`. */
  transport: NotebookTransport;
}

/** Helper: subscribe to a Tauri webview event with a sync unlisten. */
function listenWebview<T>(eventName: string, cb: (payload: T) => void): Unlisten {
  const webview = getCurrentWebview();
  let unlisten: Unlisten | null = null;
  let cancelled = false;
  webview
    .listen<T>(eventName, (event) => {
      cb(event.payload);
    })
    .then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    })
    .catch(() => {});
  return () => {
    cancelled = true;
    if (unlisten) {
      unlisten();
      unlisten = null;
    }
  };
}

export function createTauriHost(opts: CreateTauriHostOptions): NotebookHost {
  const daemon: HostDaemon = {
    async isConnected() {
      try {
        return await invoke<boolean>("is_daemon_connected");
      } catch {
        return false;
      }
    },
    async reconnect() {
      await invoke("reconnect_to_daemon");
    },
    async getInfo() {
      return invoke<DaemonInfo | null>("get_daemon_info");
    },
  };

  const blobs: HostBlobs = {
    async port() {
      return invoke<number>("get_blob_port");
    },
  };

  const trust: HostTrust = {
    async verify() {
      return invoke<TrustInfo>("verify_notebook_trust");
    },
    async approve() {
      await invoke("approve_notebook_trust");
    },
    async checkTyposquats(packages) {
      return invoke<TyposquatWarning[]>("check_typosquats", { packages });
    },
  };

  const daemonEvents: HostDaemonEvents = {
    onReady: (cb) => listenWebview<DaemonReadyPayload>("daemon:ready", cb),
    onProgress: (cb) => listenWebview<DaemonProgressPayload>("daemon:progress", cb),
    onDisconnected: (cb) => listenWebview<void>("daemon:disconnected", () => cb()),
    onUnavailable: (cb) => listenWebview<DaemonUnavailablePayload>("daemon:unavailable", cb),
    async notifySyncReady() {
      await invoke("notify_sync_ready");
    },
  };

  const notebook: HostNotebook = {
    async applyPathChanged(path) {
      await invoke("apply_path_changed", { path });
    },
    async markClean() {
      await invoke("mark_notebook_clean");
    },
  };

  const system: HostSystem = {
    async getGitInfo() {
      return invoke<GitInfo | null>("get_git_info");
    },
    async getUsername() {
      return invoke<string>("get_username");
    },
  };

  return {
    name: "tauri",
    transport: opts.transport,
    daemon,
    daemonEvents,
    blobs,
    trust,
    notebook,
    system,
  };
}
