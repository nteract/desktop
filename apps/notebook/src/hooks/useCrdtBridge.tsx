/**
 * React context and hook for cell-level CRDT bridges.
 *
 * The `CrdtBridgeProvider` is mounted once by the notebook root component,
 * giving every cell access to the WASM handle and sync trigger. Each cell
 * calls `useCrdtBridge(cellId)` to get a bridge extension wired to its
 * source text in the Automerge document.
 *
 * Inbound flow: the hook subscribes to the frame bus for text_attribution
 * broadcasts and routes them to the bridge's `applyRemoteChanges()`.
 *
 * Outbound flow: the bridge's ViewPlugin calls `splice_source` on the WASM
 * handle directly (character-level, no Myers diff). The `onSourceChanged`
 * callback updates the cell store, and `onSyncNeeded` triggers the debounced
 * sync to the daemon.
 */

import type { Extension } from "@codemirror/state";
import { createContext, type ReactNode, useContext, useEffect, useMemo, useRef } from "react";
import { type CrdtBridge, createCrdtBridge, type RemoteChange } from "../lib/crdt-editor-bridge";
import { logger } from "../lib/logger";
import { updateCellById } from "../lib/notebook-cells";
import { subscribeBroadcast } from "../lib/notebook-frame-bus";
import type { NotebookHandle } from "../wasm/runtimed-wasm/runtimed_wasm.js";

// ── Context ──────────────────────────────────────────────────────────

interface CrdtBridgeContextValue {
  /** Read the current WASM NotebookHandle (null during bootstrap). */
  getHandle: () => NotebookHandle | null;
  /** Signal that the CRDT was mutated and needs syncing to daemon. */
  onSyncNeeded: () => void;
  /** Mark the notebook as dirty (unsaved changes). */
  setDirty: (dirty: boolean) => void;
  /** Local actor label (e.g. "human:abcd1234") for filtering self-echo attributions. */
  localActor: string;
}

const CrdtBridgeContext = createContext<CrdtBridgeContextValue | null>(null);

// ── Provider ─────────────────────────────────────────────────────────

interface CrdtBridgeProviderProps {
  getHandle: () => NotebookHandle | null;
  onSyncNeeded: () => void;
  setDirty: (dirty: boolean) => void;
  localActor: string;
  children: ReactNode;
}

export function CrdtBridgeProvider({
  getHandle,
  onSyncNeeded,
  setDirty,
  localActor,
  children,
}: CrdtBridgeProviderProps) {
  // Stable ref so the context value doesn't change on every render.
  const valueRef = useRef<CrdtBridgeContextValue>({
    getHandle,
    onSyncNeeded,
    setDirty,
    localActor,
  });
  valueRef.current.getHandle = getHandle;
  valueRef.current.onSyncNeeded = onSyncNeeded;
  valueRef.current.setDirty = setDirty;
  valueRef.current.localActor = localActor;

  // The context value object itself is stable (same ref every render).
  const value = valueRef.current;

  return <CrdtBridgeContext.Provider value={value}>{children}</CrdtBridgeContext.Provider>;
}

// ── Hook ─────────────────────────────────────────────────────────────

/**
 * Create a CRDT bridge for a specific cell.
 *
 * Returns:
 * - `extension` — a CodeMirror Extension to include in the editor
 * - `bridge` — the bridge instance (for imperative access if needed)
 *
 * The hook subscribes to the frame bus for inbound text attributions
 * targeting this cell. Cleanup is automatic on unmount.
 */
export function useCrdtBridge(cellId: string): {
  extension: Extension;
  bridge: CrdtBridge;
} {
  const ctx = useContext(CrdtBridgeContext);
  if (!ctx) {
    throw new Error("useCrdtBridge must be used within a CrdtBridgeProvider");
  }

  // Stable refs so the bridge config closures always read fresh values.
  const ctxRef = useRef(ctx);
  ctxRef.current = ctx;

  const bridge = useMemo(() => {
    return createCrdtBridge({
      getHandle: () => ctxRef.current.getHandle(),
      cellId,
      onSourceChanged: (source: string) => {
        updateCellById(cellId, (c) => ({ ...c, source }));
      },
      onSyncNeeded: () => {
        ctxRef.current.onSyncNeeded();
        ctxRef.current.setDirty(true);
      },
    });
  }, [cellId]);

  // Subscribe to the frame bus for inbound text attributions.
  useEffect(() => {
    const unsubscribe = subscribeBroadcast((payload: unknown) => {
      if (
        !payload ||
        typeof payload !== "object" ||
        (payload as { type?: string }).type !== "text_attribution"
      ) {
        return;
      }

      const event = payload as {
        type: "text_attribution";
        attributions: Array<{
          cell_id: string;
          index: number;
          text: string;
          deleted: number;
          actors: string[];
        }>;
      };

      logger.debug(
        `[crdt-bridge] text_attribution broadcast received: ${event.attributions.length} attrs, looking for cell ${cellId.slice(0, 8)}`,
        event.attributions.map(
          (a) =>
            `${a.cell_id.slice(0, 8)}: idx=${a.index} del=${a.deleted} text="${a.text.slice(0, 20)}"`,
        ),
      );

      // Filter to attributions for this cell, skipping self-echo.
      // When the UI's own edits echo back through the daemon (amplified
      // by other peers' sync rounds), they produce attributions where the
      // only actor is the local human actor. CodeMirror already has these
      // changes, so applying them again would corrupt the editor state.
      const localActor = ctxRef.current.localActor;
      const changes: RemoteChange[] = [];
      for (const attr of event.attributions) {
        if (attr.cell_id !== cellId) continue;
        if (attr.actors.length === 1 && attr.actors[0] === localActor) {
          continue;
        }
        changes.push({
          index: attr.index,
          text: attr.text,
          deleted: attr.deleted,
        });
      }

      if (changes.length > 0) {
        const view = bridge.getView();
        logger.debug(
          `[crdt-bridge] Applying ${changes.length} remote changes to cell ${cellId.slice(0, 8)}, view=${view ? "attached" : "NULL"}`,
          changes,
        );
        bridge.applyRemoteChanges(changes);
      }
    });

    return unsubscribe;
  }, [cellId, bridge]);

  return { extension: bridge.extension, bridge };
}
