/**
 * Host-platform abstraction for the notebook frontend.
 *
 * `NotebookHost` is the single surface the notebook UI uses for every
 * side-effecting call that depends on where it's running — reading the
 * daemon connection, opening files, showing dialogs, listening for
 * window events, etc. In the Tauri desktop app the implementation
 * routes through `@tauri-apps/api` + plugins; in the (coming) Electron
 * host it routes through `window.electronHost` exposed by the preload
 * contextBridge; in the future a WASM-only / browser-served host could
 * implement a subset of this API and no-op the rest.
 *
 * The notebook frontend itself should never import `@tauri-apps/*`
 * directly. Every call site goes through `useNotebookHost()` or a
 * non-React helper that takes a host instance. That constraint is what
 * lets us re-host the frontend cleanly.
 *
 * ## Design notes
 *
 * - Methods return promises even when the underlying implementation is
 *   synchronous, because some hosts (notably Electron's renderer
 *   talking to main) are async and we want one signature everywhere.
 * - Event methods take a callback and return an `unlisten` function.
 *   This matches Tauri's `webview.listen()` shape.
 * - Optional namespaces (`updater`, `env`, `deps`) can be `undefined`
 *   when the host doesn't support them. Callers must null-check.
 */

import type { NotebookTransport } from "runtimed";

// ── Shared types ─────────────────────────────────────────────────────────

export interface GitInfo {
  branch: string;
  commit: string;
  description: string | null;
}

export interface DaemonInfo {
  version: string;
  socket_path: string;
  is_dev_mode: boolean;
}

export interface TrustInfo {
  status: "trusted" | "untrusted" | "signature_invalid" | "no_dependencies";
  uv_dependencies: string[];
  conda_dependencies: string[];
  conda_channels: string[];
}

export interface TyposquatWarning {
  package: string;
  similar_to: string;
  distance: number;
}

export interface DaemonReadyPayload {
  runtime?: string;
}

export interface DaemonProgressPayload {
  status: "checking" | "ready" | "failed" | string;
  error?: string;
  [key: string]: unknown;
}

export interface DaemonUnavailablePayload {
  reason: string;
  message: string;
  guidance: string;
}

export type Unlisten = () => void;

// ── Namespaces ───────────────────────────────────────────────────────────

/** Daemon connection state + diagnostics. */
export interface HostDaemon {
  /** Fast synchronous-ish check; returns false when the daemon socket is down. */
  isConnected(): Promise<boolean>;
  /** Forces a reconnect; resolves when the relay task has a fresh socket. */
  reconnect(): Promise<void>;
  /** Daemon diagnostics for banners / debug UI. */
  getInfo(): Promise<DaemonInfo | null>;
}

/** Blob store — the daemon's HTTP blob server port. */
export interface HostBlobs {
  /** Current blob server port. Implementations handle their own retry/caching. */
  port(): Promise<number>;
}

/** Notebook trust state. */
export interface HostTrust {
  verify(): Promise<TrustInfo>;
  approve(): Promise<void>;
  checkTyposquats(packages: string[]): Promise<TyposquatWarning[]>;
}

/** Lifecycle events that historically came through `webview.listen()`. */
export interface HostDaemonEvents {
  onReady(cb: (payload: DaemonReadyPayload) => void): Unlisten;
  onProgress(cb: (payload: DaemonProgressPayload) => void): Unlisten;
  onDisconnected(cb: () => void): Unlisten;
  onUnavailable(cb: (payload: DaemonUnavailablePayload) => void): Unlisten;
  /** Frontend signal that the JS frame listener is attached; relay replays any buffered frames. */
  notifySyncReady(): Promise<void>;
}

/** Notebook-scoped state transitions the UI sometimes has to announce to the host. */
export interface HostNotebook {
  /** Daemon's path for this room changed (save / save-as); flushed to window state. */
  applyPathChanged(path: string): Promise<void>;
  /** Daemon autosaved the doc; clear frontend dirty marker. */
  markClean(): Promise<void>;
}

/** Non-specific system metadata. */
export interface HostSystem {
  getGitInfo(): Promise<GitInfo | null>;
  getUsername(): Promise<string>;
}

// ── Host ──────────────────────────────────────────────────────────────────

/**
 * The top-level interface every host implementation provides.
 *
 * Transport is not optional: the notebook frontend can't run without it.
 * Everything else is grouped by concern so future PRs can add a namespace
 * without expanding the root surface.
 */
export interface NotebookHost {
  readonly name: "tauri" | "electron" | "browser" | (string & {});
  readonly transport: NotebookTransport;
  readonly daemon: HostDaemon;
  readonly daemonEvents: HostDaemonEvents;
  readonly blobs: HostBlobs;
  readonly trust: HostTrust;
  readonly notebook: HostNotebook;
  readonly system: HostSystem;
  // Future namespaces (add in dedicated PRs):
  //   settings:  HostSettings
  //   env:       HostEnv        (detect_pyproject, detect_pixi_toml, …)
  //   deps:      HostDeps       (uv / conda / pixi / deno dependency edits)
  //   dialog:    HostDialog     (plugin-dialog: open/save file pickers)
  //   shell:     HostShell      (plugin-shell: openExternal)
  //   updater?:  HostUpdater    (optional; not all hosts auto-update)
  //   window:    HostWindow     (onFocus, setTitle, …)
  //   log:       HostLog        (plugin-log pipe — host.log.debug/info/…)
}
