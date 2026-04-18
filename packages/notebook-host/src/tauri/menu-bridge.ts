/**
 * Tauri-side menu bridge — translates `webview.listen("menu:*")` events
 * into `host.commands.run(...)` invocations.
 *
 * The app registers command handlers; this bridge makes sure that Tauri
 * menu clicks end up calling them. Future hosts (Electron, browser with
 * a palette UI) bind their own input surfaces to the same command ids.
 */

import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { CommandId, CommandPayloads } from "../commands";
import type { NotebookHost } from "../types";

type BindEntry<K extends CommandId> = {
  menuEvent: string;
  commandId: K;
  /**
   * Transform the raw event payload into the command's payload. Return
   * `null` to ignore the event entirely (e.g., an unrecognized
   * `menu:insert-cell` cell type). `undefined` is a valid payload for
   * void commands and is passed through.
   */
  parse?: (ev: unknown) => CommandPayloads[K] | null;
};

/** Fire-and-forget subscription to a Tauri webview event. */
function listenMenu(eventName: string, cb: (payload: unknown) => void): () => void {
  const webview = getCurrentWebview();
  let unlisten: (() => void) | null = null;
  let cancelled = false;
  webview
    .listen<unknown>(eventName, (ev) => {
      cb(ev.payload);
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

/**
 * Wire up every known `menu:*` event to its command id. Returns a
 * disposer that unlistens from all of them.
 *
 * `createTauriHost()` invokes this once after constructing the registry,
 * so the app itself never sees `menu:*` traffic.
 */
export function wireTauriMenuBridge(host: NotebookHost): () => void {
  function bind<K extends CommandId>(entry: BindEntry<K>): () => void {
    return listenMenu(entry.menuEvent, (payload) => {
      let cmdPayload: CommandPayloads[K];
      if (entry.parse) {
        const parsed = entry.parse(payload);
        if (parsed === null) {
          // Malformed payload — skip rather than silently coerce.
          console.warn(`[menu-bridge] ${entry.menuEvent}: unrecognized payload, skipping`, payload);
          return;
        }
        cmdPayload = parsed;
      } else {
        cmdPayload = undefined as unknown as CommandPayloads[K];
      }
      host.commands.run(entry.commandId, cmdPayload).catch((err) => {
        console.error(`[menu-bridge] ${entry.menuEvent} → ${entry.commandId} failed:`, err);
      });
    });
  }

  const disposables: Array<() => void> = [
    bind({ menuEvent: "menu:save", commandId: "notebook.save" }),
    bind({ menuEvent: "menu:open", commandId: "notebook.open" }),
    bind({ menuEvent: "menu:clone", commandId: "notebook.clone" }),
    bind({
      menuEvent: "menu:insert-cell",
      commandId: "notebook.insertCell",
      parse: (ev) => {
        if (ev === "code" || ev === "markdown" || ev === "raw") return { type: ev };
        return null; // unknown cell type — drop the event
      },
    }),
    bind({ menuEvent: "menu:clear-outputs", commandId: "notebook.clearOutputs" }),
    bind({ menuEvent: "menu:clear-all-outputs", commandId: "notebook.clearAllOutputs" }),
    bind({ menuEvent: "menu:run-all", commandId: "notebook.runAll" }),
    bind({ menuEvent: "menu:restart-and-run-all", commandId: "notebook.restartAndRunAll" }),
    bind({ menuEvent: "menu:check-for-updates", commandId: "updater.check" }),

    // Zoom is host chrome, not a notebook command. Handle directly via
    // `webview.setZoom()`. Min 0.5×, max 3.0×, step 0.1. Each subscription
    // tracks its own zoom level in closure state — one webview, one level.
    ...(() => {
      const webview = getCurrentWebview();
      let zoom = 1.0;
      const setZoom = (next: number) => {
        zoom = Math.min(3.0, Math.max(0.5, next));
        webview.setZoom(zoom);
      };
      return [
        listenMenu("menu:zoom-in", () => setZoom(zoom + 0.1)),
        listenMenu("menu:zoom-out", () => setZoom(zoom - 0.1)),
        listenMenu("menu:zoom-reset", () => setZoom(1.0)),
      ];
    })(),
  ];

  return () => disposables.forEach((d) => d());
}
