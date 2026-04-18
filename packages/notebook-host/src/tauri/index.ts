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

import { invoke, isTauri } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import {
  attachConsole as pluginAttachConsole,
  debug as pluginDebug,
  error as pluginError,
  info as pluginInfo,
  warn as pluginWarn,
} from "@tauri-apps/plugin-log";
import type { NotebookTransport } from "runtimed";
import { createCommandRegistry } from "../commands";
import { wireTauriMenuBridge } from "./menu-bridge";
import { TauriTransport } from "./transport";

import type {
  DaemonInfo,
  DaemonProgressPayload,
  DaemonReadyPayload,
  DaemonUnavailablePayload,
  GitInfo,
  HostBlobs,
  HostDaemon,
  HostDaemonEvents,
  HostDeps,
  HostLog,
  HostNotebook,
  HostRelay,
  HostSystem,
  HostTrust,
  NotebookHost,
  TrustInfo,
  TyposquatWarning,
  Unlisten,
} from "../types";

export interface CreateTauriHostOptions {
  /**
   * Override the `NotebookTransport`. Defaults to a fresh `TauriTransport`
   * construction, which is what the desktop app should use at boot.
   * Provide a custom instance for tests or multi-transport scenarios.
   */
  transport?: NotebookTransport;
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

export function createTauriHost(opts: CreateTauriHostOptions = {}): NotebookHost {
  const transport = opts.transport ?? new TauriTransport();
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
  };

  const deps: HostDeps = {
    async checkTyposquats(packages) {
      return invoke<TyposquatWarning[]>("check_typosquats", { packages });
    },
  };

  const daemonEvents: HostDaemonEvents = {
    onReady: (cb) => listenWebview<DaemonReadyPayload>("daemon:ready", cb),
    onProgress: (cb) => listenWebview<DaemonProgressPayload>("daemon:progress", cb),
    onDisconnected: (cb) => listenWebview<void>("daemon:disconnected", () => cb()),
    onUnavailable: (cb) => listenWebview<DaemonUnavailablePayload>("daemon:unavailable", cb),
  };

  const relay: HostRelay = {
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

  const commands = createCommandRegistry();

  // plugin-log always resolves; fire-and-forget so callers stay sync.
  // `isTauri()` guards the one-time `attachConsole()` for tests and SSR.
  const log: HostLog = {
    debug(message) {
      pluginDebug(message).catch(() => {});
    },
    info(message) {
      pluginInfo(message).catch(() => {});
    },
    warn(message) {
      pluginWarn(message).catch(() => {});
    },
    error(message) {
      pluginError(message).catch(() => {});
    },
  };
  // In a real Tauri window, mirror plugin-log output to the browser console
  // so devtools shows it alongside Rust-side entries. Safe to call outside
  // Tauri — the plugin no-ops when IPC isn't available.
  if (isTauri() && import.meta.env.DEV) {
    pluginAttachConsole().catch(() => {});
  }

  const host: NotebookHost = {
    name: "tauri",
    transport,
    daemon,
    daemonEvents,
    relay,
    blobs,
    trust,
    deps,
    notebook,
    system,
    commands,
    log,
  };

  // Wire Tauri menu events into the command registry. Stash the disposer
  // on the module so hot-reload / multi-host test teardown can reclaim
  // the listeners. For production single-session lifetime this is
  // unreachable, but dropping the disposer entirely leaks on any future
  // lifecycle change.
  _lastMenuBridgeDispose?.();
  _lastMenuBridgeDispose = wireTauriMenuBridge(host);

  return host;
}

/**
 * Internal: last menu-bridge disposer. If `createTauriHost()` is called
 * more than once (hot reload, tests), we dispose the previous bridge
 * before wiring a new one. Intentionally module-scoped — the host API
 * stays clean until we add a full `host.dispose()` lifecycle.
 */
let _lastMenuBridgeDispose: (() => void) | undefined;

/** @internal Test helper — forget the most recent menu bridge disposer. */
export function _resetMenuBridgeForTests(): void {
  _lastMenuBridgeDispose?.();
  _lastMenuBridgeDispose = undefined;
}

export { TauriTransport } from "./transport";
