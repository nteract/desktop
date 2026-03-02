/**
 * Debug-flag-aware logger for the notebook app.
 *
 * In development (import.meta.env.DEV), all log levels are enabled.
 * In production, debug logs are suppressed unless explicitly enabled
 * via localStorage: `localStorage.setItem('runt:debug', 'true')`
 */

const isDebugEnabled = (): boolean => {
  if (import.meta.env.DEV) return true;
  try {
    return localStorage.getItem("runt:debug") === "true";
  } catch {
    return false;
  }
};

export const logger = {
  /**
   * Debug-level logging. Suppressed in production unless runt:debug is enabled.
   * Use for routine operations, per-cell execution, retry attempts, etc.
   */
  debug: (...args: unknown[]): void => {
    if (isDebugEnabled()) {
      console.debug(...args);
    }
  },

  /**
   * Info-level logging. Always enabled.
   * Use for significant user-triggered actions (shutdown, sync, etc.)
   */
  info: (...args: unknown[]): void => {
    console.log(...args);
  },

  /**
   * Warning-level logging. Always enabled.
   * Use for recoverable issues that may indicate problems.
   */
  warn: (...args: unknown[]): void => {
    console.warn(...args);
  },

  /**
   * Error-level logging. Always enabled.
   * Use for failures that affect functionality.
   */
  error: (...args: unknown[]): void => {
    console.error(...args);
  },
};
