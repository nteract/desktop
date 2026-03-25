/**
 * Unified logger for the notebook app.
 *
 * Routes frontend logs through @tauri-apps/plugin-log so they appear
 * in notebook.log alongside Rust-side log::* entries. Also forwards
 * to the browser console in development for devtools convenience.
 *
 * All methods are synchronous (fire-and-forget) so callers don't need
 * to await — the IPC call happens in the background.
 */

import { isTauri } from "@tauri-apps/api/core";
import {
  attachConsole,
  debug as logDebug,
  error as logError,
  info as logInfo,
  warn as logWarn,
} from "@tauri-apps/plugin-log";

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

/** Whether Tauri IPC is available (false in Vitest, Storybook, etc.) */
const hasTauri = isTauri();

/** Fire-and-forget wrapper — log calls are async IPC but callers stay sync. */
function send(fn: (message: string) => Promise<void>, args: unknown[]): void {
  if (!hasTauri) {
    // Fall back to console when Tauri isn't available (tests, SSR)
    console.log(formatArgs(args));
    return;
  }
  fn(formatArgs(args)).catch(() => {});
}

export const logger = {
  /**
   * Debug-level logging. Visible in notebook.log when RUST_LOG=debug.
   * Use for routine operations, per-cell execution, retry attempts, etc.
   */
  debug: (...args: unknown[]): void => {
    send(logDebug, args);
  },

  /**
   * Info-level logging. Always enabled.
   * Use for significant user-triggered actions (shutdown, sync, etc.)
   */
  info: (...args: unknown[]): void => {
    send(logInfo, args);
  },

  /**
   * Warning-level logging. Always enabled.
   * Use for recoverable issues that may indicate problems.
   */
  warn: (...args: unknown[]): void => {
    send(logWarn, args);
  },

  /**
   * Error-level logging. Always enabled.
   * Use for failures that affect functionality.
   */
  error: (...args: unknown[]): void => {
    send(logError, args);
  },
};

// In development, also forward all logs to the browser console so
// devtools show them alongside the file output. Guard against test
// environments where Tauri internals aren't available.
if (import.meta.env.DEV && hasTauri) {
  attachConsole();
}
