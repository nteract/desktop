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
  notebook_id?: string;
  cell_count?: number;
  needs_trust_approval?: boolean;
  /** In-memory-only notebook (no on-disk path). Drives the always-dirty asterisk. */
  ephemeral?: boolean;
  /** On-disk path if the notebook is file-backed. Derives the titlebar filename. */
  notebook_path?: string | null;
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
  /**
   * Pull the most-recent `daemon:ready` payload for this window, or null if
   * one hasn't landed yet. Used by late-mounted consumers to backfill state
   * that was emitted before any JS listener was attached — Tauri webview
   * events aren't sticky, so the event-based path can miss the first fire.
   */
  getReadyInfo(): Promise<DaemonReadyPayload | null>;
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
 * (useDependencies, useCondaDependencies, usePixiDetection,
 * useDenoConfig) off direct `invoke(...)`. `checkTyposquats`
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

/**
 * Notebook-scoped state transitions the UI sometimes has to announce to the host.
 *
 * NOTE: the `applyPathChanged` / `markClean` pair is a legacy shadow of state
 * the daemon already owns (last-saved heads vs current heads for dirty; the
 * `NotebookSession` for path). We keep them on the host for now because the
 * Tauri Rust side still maintains a `WindowNotebookRegistry` that the window
 * title setter reads from. Once the frontend reads path + dirty directly from
 * daemon state / RuntimeStateDoc, these two methods and their Tauri commands
 * (`apply_path_changed`, `mark_notebook_clean`, `has_notebook_path`) all go.
 */
export interface HostNotebook {
  /** Daemon's path for this room changed (save / save-as); flushed to window state. */
  applyPathChanged(path: string): Promise<void>;
  /** Daemon autosaved the doc; clear frontend dirty marker. */
  markClean(): Promise<void>;
}

/**
 * OS window chrome. Read/write the title (for the dirty asterisk) and
 * subscribe to focus changes (for keyboard-input-context restoration on
 * WKWebView reactivation).
 *
 * `onFocusChange` is the host-level / OS-attributed focus signal. The
 * frontend also listens to the DOM-level `window.addEventListener("focus")`
 * for a fast path; these are belt-and-suspenders layers.
 */
export interface HostWindow {
  getTitle(): Promise<string>;
  setTitle(title: string): Promise<void>;
  onFocusChange(cb: (focused: boolean) => void): Unlisten;
}

/** Non-specific system metadata. */
export interface HostSystem {
  getGitInfo(): Promise<GitInfo | null>;
  getUsername(): Promise<string>;
}

/** File picker. Returned paths are platform-native strings, or null if cancelled. */
export interface HostDialog {
  /** Open an existing file; returns the selected path or null on cancel. */
  openFile(options?: HostDialogOpenOptions): Promise<string | null>;
  /** Open a save-as dialog; returns the chosen path or null on cancel. */
  saveFile(options?: HostDialogSaveOptions): Promise<string | null>;
}

/** Filter for file pickers. Matches Tauri's shape for drop-in compatibility. */
export interface HostDialogFilter {
  name: string;
  extensions: string[];
}

export interface HostDialogOpenOptions {
  filters?: HostDialogFilter[];
  defaultPath?: string;
  multiple?: false;
}

export interface HostDialogSaveOptions {
  filters?: HostDialogFilter[];
  defaultPath?: string;
}

/**
 * Open a URL in the user's default browser or handler app.
 *
 * Named "externalLinks" (not "shell") on purpose — the Tauri `plugin-shell`
 * exposes a generic shell-command surface that we deliberately don't want
 * to advertise through the host interface. URL-opening is the only subset
 * the notebook frontend uses.
 */
export interface HostExternalLinks {
  open(url: string): Promise<void>;
}

/**
 * Auto-update information for the host app itself.
 *
 * The notebook frontend polls `check()` to decide whether to show an
 * "Update available" banner. Actually installing the update (downloading
 * the payload, relaunching) is a coordinated operation that stays on
 * the host side — the frontend signals "start the upgrade flow" via a
 * separate command rather than driving download/install directly.
 */
export interface HostUpdater {
  /** Returns available update info or null when the app is up to date. */
  check(): Promise<HostUpdateInfo | null>;
}

export interface HostUpdateInfo {
  version: string;
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
  readonly window: HostWindow;
  readonly system: HostSystem;
  readonly dialog: HostDialog;
  readonly externalLinks: HostExternalLinks;
  readonly updater: HostUpdater;
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
