/**
 * Unified logger for the notebook app.
 *
 * Delegates to the `NotebookHost` installed at boot (`setLoggerHost`).
 * The host's `log` namespace decides where entries actually go — Tauri's
 * impl pipes to `@tauri-apps/plugin-log` so they appear in notebook.log
 * alongside Rust-side `log::*` entries; other hosts pick their own sink.
 *
 * All methods are synchronous (fire-and-forget) so callers don't need
 * to await — the transport to the sink happens in the background.
 */

import type { NotebookHost } from "@nteract/notebook-host";

/** Serialize arguments to a single log message string. */
function formatArgs(args: unknown[]): string {
  return args
    .map((a) => {
      if (typeof a === "string") return a;
      // Preserve Error messages and stacks (JSON.stringify(Error) is just "{}")
      if (a instanceof Error) return a.stack ?? `${a.name}: ${a.message}`;
      try {
        return JSON.stringify(a);
      } catch {
        // Circular references, proxies, etc.
        return String(a);
      }
    })
    .join(" ");
}

// Host is installed by `apps/notebook/src/main.tsx` right after
// `createTauriHost()`. Until then — and in tests, SSR, Storybook — we
// fall back to the console. Never throw from logger; it's used at the
// error-handling boundary.
let _host: NotebookHost | null = null;

/** Install the `NotebookHost` whose `log` surface this logger writes to. */
export function setLoggerHost(host: NotebookHost | null): void {
  _host = host;
}

function emit(level: "debug" | "info" | "warn" | "error", args: unknown[]): void {
  const message = formatArgs(args);
  if (_host) {
    _host.log[level](message);
  } else {
    // Pre-host or non-host contexts: mirror to console with a level tag.
    // eslint-disable-next-line no-console
    console.log(`[${level}] ${message}`);
  }
}

export const logger = {
  /**
   * Debug-level logging. Visible in notebook.log when RUST_LOG=debug.
   * Use for routine operations, per-cell execution, retry attempts, etc.
   */
  debug: (...args: unknown[]): void => emit("debug", args),

  /**
   * Info-level logging. Always enabled.
   * Use for significant user-triggered actions (shutdown, sync, etc.)
   */
  info: (...args: unknown[]): void => emit("info", args),

  /**
   * Warning-level logging. Always enabled.
   * Use for recoverable issues that may indicate problems.
   */
  warn: (...args: unknown[]): void => emit("warn", args),

  /**
   * Error-level logging. Always enabled.
   * Use for failures that affect functionality.
   */
  error: (...args: unknown[]): void => emit("error", args),
};
