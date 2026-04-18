/**
 * `CommandRegistry` — the contract between the app and whatever UI surfaces
 * fire user intents (menus, keyboard shortcuts, a future command palette, MCP).
 *
 * The app side registers handlers by name. The host side routes its native
 * UI (Tauri menu events, Electron menu clicks, web keyboard bindings) into
 * `commands.run(id, payload)`. Menu integration becomes host code and stops
 * leaking through the app.
 *
 * Why a typed command map instead of loose strings: every caller gets
 * compile-time assurance that the command exists and the payload matches.
 * Renaming a command updates every binding in one go; misspelling one
 * surfaces immediately.
 */

/**
 * Every command the app handles, keyed to its payload type.
 *
 * Add a new command here and TypeScript will require the registrar to
 * supply a handler and every caller to pass the right payload. Removing
 * one will require the same sweep in reverse.
 *
 * Keep the namespace prefix (`notebook.*`, `view.*`, `updater.*`, …)
 * stable — it doubles as a search index and mirrors how VSCode and
 * JupyterLab name their internal actions.
 */
export interface CommandPayloads {
  // Notebook-level actions — require app state (focused cell, handle, path).
  "notebook.save": void;
  "notebook.open": void;
  "notebook.clone": void;
  "notebook.insertCell": { type: "code" | "markdown" | "raw" };
  "notebook.clearOutputs": void;
  "notebook.clearAllOutputs": void;
  "notebook.runAll": void;
  "notebook.restartAndRunAll": void;

  // Updater — triggers the app's auto-updater check flow.
  "updater.check": void;

  // NOTE: Zoom is NOT a command. It's handled entirely host-side (Tauri
  // menu bridge calls `webview.setZoom()` directly). Routing zoom through
  // the registry would let apps override it, but in practice every host
  // controls its own viewport scaling. Keep it off the contract.
}

export type CommandId = keyof CommandPayloads;

export type CommandHandler<K extends CommandId> = (
  payload: CommandPayloads[K],
) => void | Promise<void>;

export interface CommandRegistry {
  /**
   * Register a handler for `id`. Returns a disposer that removes the
   * handler. Duplicate registration for the same id throws — two owners
   * for the same command is almost always a bug.
   */
  register<K extends CommandId>(id: K, handler: CommandHandler<K>): () => void;

  /**
   * Invoke the handler registered for `id`. Resolves with the handler's
   * return value (or `undefined` for sync handlers). If no handler is
   * registered, logs a warning and resolves without error — menus should
   * not crash the app if a command happens to fire before mount.
   */
  run<K extends CommandId>(id: K, payload: CommandPayloads[K]): Promise<void>;

  /** Snapshot of currently-registered IDs. Useful for diagnostics. */
  list(): CommandId[];
}

export function createCommandRegistry(): CommandRegistry {
  const handlers = new Map<CommandId, CommandHandler<CommandId>>();

  return {
    register<K extends CommandId>(id: K, handler: CommandHandler<K>): () => void {
      if (handlers.has(id)) {
        throw new Error(
          `CommandRegistry: handler already registered for "${id}". ` +
            `Check for duplicate useEffect registrations or stale unmounts.`,
        );
      }
      handlers.set(id, handler as CommandHandler<CommandId>);
      return () => {
        // Only dispose if this handler is still the registered one —
        // avoids accidentally un-registering a replacement handler after
        // React remount cycles.
        if (handlers.get(id) === handler) handlers.delete(id);
      };
    },

    async run<K extends CommandId>(id: K, payload: CommandPayloads[K]): Promise<void> {
      const handler = handlers.get(id) as CommandHandler<K> | undefined;
      if (!handler) {
        console.warn(`CommandRegistry: no handler registered for "${id}" — ignoring`);
        return;
      }
      await handler(payload);
    },

    list(): CommandId[] {
      return Array.from(handlers.keys());
    },
  };
}
