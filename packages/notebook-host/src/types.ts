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
 * - For PR 1 / PR 2 / PR 3 the interface commits to "Tauri and Electron
 *   both implement every namespace end-to-end." If a future browser or
 *   viewer host needs partial implementations, we'll introduce explicit
 *   capability flags or mark individual namespaces optional at that
 *   point — not pre-optional now, to keep the contract honest.
 */

import type { NotebookTransport } from "runtimed";
import type { CommandRegistry } from "./commands";

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

/** Notebook trust state (attestation + approval, nothing else). */
export interface HostTrust {
  verify(): Promise<TrustInfo>;
  approve(): Promise<void>;
}

/**
 * Dependency-validation surface.
 *
 * This namespace will grow as we migrate dep-edit flows
 * (useDependencies, useCondaDependencies, usePixiDependencies,
 * useDenoDependencies) off direct `invoke(...)`. `checkTyposquats`
 * lives here and not in `HostTrust` because it validates package
 * names, not notebook attestation.
 */
export interface HostDeps {
  checkTyposquats(packages: string[]): Promise<TyposquatWarning[]>;
}

/**
 * Subscribe-only daemon lifecycle events. These historically came
 * through `webview.listen(...)`. Return an `Unlisten` from each
 * subscription; outgoing signals belong on `HostRelay`, not here.
 */
export interface HostDaemonEvents {
  onReady(cb: (payload: DaemonReadyPayload) => void): Unlisten;
  onProgress(cb: (payload: DaemonProgressPayload) => void): Unlisten;
  onDisconnected(cb: () => void): Unlisten;
  onUnavailable(cb: (payload: DaemonUnavailablePayload) => void): Unlisten;
}

/**
 * Outbound signals the frontend sends up to the host for sync
 * bookkeeping. Separate from `HostDaemonEvents` because these are
 * commands, not subscriptions.
 */
export interface HostRelay {
  /**
   * Signal that the JS frame listener is attached and the Tauri-side
   * relay may replay any buffered frames. Matches the existing
   * `notify_sync_ready` Tauri command. No-op in hosts where the main
   * process doesn't buffer (e.g., a browser-served host).
   */
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

/**
 * Structured-log pipe shared across the frontend. Replaces the direct
 * `@tauri-apps/plugin-log` coupling in `apps/notebook/src/lib/logger.ts`.
 *
 * Messages arrive pre-formatted (single string); callers serialize their
 * arguments in a way that matters to them. The Tauri impl forwards each
 * level to plugin-log; an Electron impl can pipe to the main process log
 * file; a browser impl can use `console.*` or a remote sink.
 */
export interface HostLog {
  debug(message: string): void;
  info(message: string): void;
  warn(message: string): void;
  error(message: string): void;
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
  readonly relay: HostRelay;
  readonly blobs: HostBlobs;
  readonly trust: HostTrust;
  readonly deps: HostDeps;
  readonly notebook: HostNotebook;
  readonly system: HostSystem;
  /**
   * Typed action bus shared between host UI surfaces (menus, keyboard,
   * future palette) and the app. Host-side wiring calls `run(id, payload)`;
   * the app registers handlers via `register(id, fn)`. See `./commands.ts`
   * for the command map.
   */
  readonly commands: CommandRegistry;
  /**
   * Structured logging pipe. Tauri routes through plugin-log so entries
   * appear in notebook.log alongside Rust-side log::* entries; other hosts
   * pick their own sink.
   */
  readonly log: HostLog;
  // Future namespaces (add in dedicated PRs):
  //   settings:   HostSettings
  //   env:        HostEnv         (detect_pyproject, detect_pixi_toml, …)
  //   deps (ext): dependency *edit* APIs — this PR only has validation
  //   dialog:     HostDialog      (plugin-dialog: open/save file pickers)
  //   externalLinks: HostExternalLinks (plugin-shell.open — opening URLs,
  //                                     NOT a shell surface)
  //   updater?:   HostUpdater     (optional; not all hosts auto-update)
  //   window:     HostWindow      (onFocus, setTitle, …)
  //   log:        HostLog         (plugin-log pipe — host.log.debug/info/…)
}
