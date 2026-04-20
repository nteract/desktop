/**
 * FULL TREE INTEGRATION — uses the REAL NotebookView + CodeCell from
 * apps/notebook/src/ in a browser context (no Tauri).
 *
 * RUNTIME COUPLING ANALYSIS (all discovered edges):
 *
 * 1. CrdtBridgeProvider (REQUIRED) — CodeCell calls useCrdtBridge(cellId)
 *    which throws "must be used within CrdtBridgeProvider" without context.
 *    We provide the real WASM handle; outbound path no-ops when read-only.
 *
 * 2. PresenceProvider (REQUIRED) — wraps usePresence which calls
 *    useNotebookHost().transport.sendFrame(). Must be inside NotebookHostProvider.
 *    Safe at mount — WASM encode functions only fire on user interaction.
 *
 * 3. NotebookHostProvider (REQUIRED) — PresenceProvider, kernel-completion,
 *    history-search all call useNotebookHost(). We provide createBrowserHost()
 *    with the WebSocket transport.
 *
 * 4. Module-level cell store (REQUIRED) — NotebookView reads from
 *    useCellIds()/useCell(id). We populate via replaceNotebookCells() when
 *    WASM handle syncs.
 *
 * 5. cell-ui-state (REQUIRED) — CodeCell reads useIsCellFocused/Executing/
 *    Queued. We drive via setExecutingCellIds/setQueuedCellIds from
 *    RuntimeState subscription. flushCellUIState() called each render.
 *
 * 6. EditorRegistryProvider — NotebookView wraps its content in this
 *    internally (line 811 of NotebookView.tsx). No external provision needed.
 *
 * 7. cursor-registry.ts — module-level, subscribes via startCursorDispatch()
 *    which is NOT called at import time. Safe without explicit activation.
 *
 * 8. kernel-completion — module-level _host is null, completion source
 *    early-returns. No crash.
 *
 * 9. HistorySearchDialog — lazy-loaded, only triggers on Ctrl+R. Uses
 *    useNotebookHost() which we provide.
 *
 * 10. IsolatedRendererProvider + WidgetStoreProvider — needed for OutputArea
 *     iframe isolation and widget models.
 *
 * REMAINING WALLS (not solvable without code changes):
 * - Read-only: no local edits (CrdtBridge outbound path never fires)
 * - No kernel execution (onExecuteCell is no-op)
 * - No drag-and-drop reordering persisted (onMoveCell is no-op)
 * - WASM encode functions (presence) work but send to no peer
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { NotebookHostProvider } from "@nteract/notebook-host";
import { createBrowserHost } from "@nteract/notebook-host/browser";
import { WidgetStoreProvider } from "@/components/widgets/widget-store-context";
import { IsolatedRendererProvider } from "@/components/isolated/isolated-renderer-context";
import { SyncEngine } from "runtimed/src/sync-engine";
import type { SyncableHandle } from "runtimed/src/handle";
import type { RuntimeState } from "runtimed/src/runtime-state";
import init, { NotebookHandle } from "runtimed-wasm/runtimed_wasm.js";
import { WebSocketTransport } from "~/lib/ws-transport";

// ─── THE REAL IMPORTS ───────────────────────────────────────────────────
// These come from apps/notebook/src/ — the actual notebook app code.

// Cell store (module-level singletons — shared across components)
import {
  useCellIds,
  replaceNotebookCells,
} from "notebook-app/lib/notebook-cells";


// Cell UI state (focus, executing, queued)
import {
  setFocusedCellId,
  setExecutingCellIds,
  setQueuedCellIds,
  flushCellUIState,
} from "notebook-app/lib/cell-ui-state";


// The actual NotebookView component with dnd-kit, stable DOM order, etc.
import { NotebookView } from "notebook-app/components/NotebookView";

// CrdtBridgeProvider — CodeCell throws without this context
import { CrdtBridgeProvider } from "notebook-app/hooks/useCrdtBridge";

// PresenceProvider — CodeCell uses usePresenceContext (returns null safely)
import { PresenceProvider } from "notebook-app/contexts/PresenceContext";

// Types
import type { NotebookCell } from "notebook-app/types";

interface Props {
  notebookId: string;
}

/**
 * FullTreeViewer — attempts to use the real NotebookView component.
 *
 * COUPLING EDGES DISCOVERED (runtime):
 * - NotebookView requires cellIds to be populated in the module-level store
 * - NotebookView requires runtime prop (kernel language for syntax highlighting)
 * - CodeCell requires useCrdtBridge which needs a WASM handle in module scope
 * - CodeCell calls useIsCellFocused/useIsCellExecuting from cell-ui-state
 * - NotebookView auto-seeds first cell on empty notebooks (onAddCell)
 * - NotebookView expects all callbacks (onExecuteCell, onDeleteCell, etc.)
 */
export function FullTreeViewer({ notebookId }: Props) {
  const [status, setStatus] = useState<"connecting" | "syncing" | "live" | "error">("connecting");
  const [kernelStatus, setKernelStatus] = useState<string>("");
  const [transport, setTransport] = useState<WebSocketTransport | null>(null);
  const engineRef = useRef<SyncEngine | null>(null);
  const transportRef = useRef<WebSocketTransport | null>(null);
  const handleRef = useRef<SyncableHandle | null>(null);

  // Track cell IDs from the store
  const cellIds = useCellIds();

  useEffect(() => {
    let disposed = false;

    async function initViewer() {
      await init();
      if (disposed) return;

      const handle = NotebookHandle.create_bootstrap("live-viewer");
      handleRef.current = handle as unknown as SyncableHandle;

      const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
      const url = `${proto}//${window.location.host}/ws/join?id=${encodeURIComponent(notebookId)}`;

      const ws = new WebSocketTransport({
        url,
        onOpen: () => setStatus("syncing"),
        onClose: () => {
          if (!disposed) setStatus("error");
        },
      });
      transportRef.current = ws;
      setTransport(ws);

      const engine = new SyncEngine({
        getHandle: () => handleRef.current,
        transport: ws,
        logger: {
          debug: () => {},
          info: console.info.bind(console),
          warn: console.warn.bind(console),
          error: console.error.bind(console),
        },
      });
      engineRef.current = engine;

      // When cells change, materialize into the module-level store
      // This is what useAutomergeNotebook does internally
      engine.cellChanges$.subscribe(() => {
        if (disposed) return;
        materializeCells();
        setStatus("live");
      });

      engine.initialSyncComplete$.subscribe(() => {
        if (disposed) return;
        setStatus("live");
        materializeCells();
      });

      engine.runtimeState$.subscribe((state: RuntimeState) => {
        if (disposed) return;
        setKernelStatus(state.kernel.status);

        // Drive the cell-ui-state store with execution info
        const executing = new Set<string>();
        const queued = new Set<string>();
        for (const exec of Object.values(state.executions)) {
          if (exec.status === "running") executing.add(exec.cell_id);
          if (exec.status === "queued") queued.add(exec.cell_id);
        }
        setExecutingCellIds(executing);
        setQueuedCellIds(queued);

        // Re-materialize when runtime state changes (execution counts updated)
        materializeCells();
      });

      engine.start();
    }

    function materializeCells() {
      const handle = handleRef.current;
      if (!handle) return;

      try {
        const h = handle as unknown as NotebookHandle;
        const cellIds: string[] = h.get_cell_ids();
        const cells: NotebookCell[] = [];
        for (const id of cellIds) {
          const cellType = h.get_cell_type(id);
          if (!cellType) continue;
          const source = h.get_cell_source(id) ?? "";
          const metadata = h.get_cell_metadata(id) ?? {};

          if (cellType === "code") {
            const rawOutputs: unknown[] = h.get_cell_outputs(id) ?? [];
            const outputs = rawOutputs
              .map(resolveOutput)
              .filter((o): o is NonNullable<typeof o> => o !== null);
            cells.push({
              id, cell_type: "code", source,
              execution_count: null, outputs, metadata,
            });
          } else if (cellType === "markdown") {
            cells.push({ id, cell_type: "markdown", source, metadata });
          } else {
            cells.push({ id, cell_type: "raw", source, metadata });
          }
        }
        replaceNotebookCells(cells);
      } catch {
        // Handle may not have synced yet
      }
    }

    // Resolve a raw output from WASM into a JupyterOutput.
    // ContentRefs with blob hashes → same-origin /blob/{hash} URLs.
    function resolveOutput(output: unknown): any | null {
      if (typeof output !== "object" || output === null) return null;
      const obj = output as Record<string, unknown>;
      if (!("output_type" in obj)) return null;

      if (obj.output_type === "stream") {
        const text = resolveRef(obj.text);
        if (text === null) return null;
        return { output_type: "stream", name: obj.name, text };
      }
      if (obj.output_type === "error") {
        let traceback: string[];
        if (Array.isArray(obj.traceback)) {
          traceback = obj.traceback as string[];
        } else {
          const tb = resolveRef(obj.traceback);
          if (tb === null) return null;
          try {
            const parsed = JSON.parse(tb);
            traceback = Array.isArray(parsed) ? parsed : [String(parsed)];
          } catch {
            traceback = [tb];
          }
        }
        return {
          output_type: "error",
          ename: String(obj.ename ?? ""),
          evalue: String(obj.evalue ?? ""),
          traceback,
        };
      }
      if (obj.output_type === "display_data" || obj.output_type === "execute_result") {
        const data = obj.data as Record<string, unknown> | undefined;
        if (!data) return null;
        const resolved: Record<string, unknown> = {};
        for (const [mime, ref_] of Object.entries(data)) {
          const val = resolveRef(ref_);
          if (val === null) continue;
          if (mime.includes("json")) {
            try { resolved[mime] = JSON.parse(val); } catch { resolved[mime] = val; }
          } else {
            resolved[mime] = val;
          }
        }
        if (Object.keys(resolved).length === 0) return null;
        return {
          output_type: obj.output_type, data: resolved,
          metadata: obj.metadata ?? {},
          execution_count: obj.execution_count ?? null,
        };
      }
      return obj;
    }

    function resolveRef(ref_: unknown): string | null {
      if (typeof ref_ === "string") return ref_;
      if (typeof ref_ !== "object" || ref_ === null) return null;
      const r = ref_ as Record<string, unknown>;
      if ("inline" in r && typeof r.inline === "string") return r.inline;
      if ("url" in r && typeof r.url === "string") return r.url;
      if ("blob" in r && typeof r.blob === "string")
        return `${window.location.origin}/blob/${r.blob}`;
      return null;
    }

    initViewer();

    return () => {
      disposed = true;
      engineRef.current?.stop();
      transportRef.current?.disconnect();
      const h = handleRef.current as unknown as { free?(): void } | null;
      h?.free?.();
    };
  }, [notebookId]);

  // Flush cell-ui-state on every render (normally done by AppContent's useLayoutEffect)
  useEffect(() => {
    flushCellUIState();
  });

  const host = useMemo(() => {
    if (!transport) return null;
    return createBrowserHost({ transport });
  }, [transport]);

  // ─── COUPLING EDGE: NotebookView expects all these callbacks ────────
  // In the real app, these come from useAutomergeNotebook + useDaemonKernel.
  // For read-only mode, they're no-ops.
  const noop = () => {};
  const noopString = (_id: string) => {};

  const content = (
    <div className="w-full">
      <div className="mb-3 flex items-center gap-2 px-4 py-2 text-xs text-muted-foreground">
        <span
          className={`h-2 w-2 rounded-full ${
            status === "live" ? "bg-green-500" : status === "error" ? "bg-red-500" : "bg-blue-500 animate-pulse"
          }`}
        />
        <span>{status}</span>
        {kernelStatus && (
          <>
            <span className="text-border">·</span>
            <span>kernel: {kernelStatus}</span>
          </>
        )}
        <span className="text-border">·</span>
        <span>{cellIds.length} cells</span>
      </div>

      {cellIds.length === 0 && status === "live" && (
        <div className="py-12 text-center text-muted-foreground">Empty notebook</div>
      )}

      {cellIds.length > 0 && (
        <NotebookView
          cellIds={cellIds}
          isLoading={status !== "live"}
          runtime="python"
          onFocusCell={(id) => setFocusedCellId(id)}
          onExecuteCell={noopString}
          onInterruptKernel={noop}
          onDeleteCell={noopString}
          onAddCell={noop as any}
          onMoveCell={noopString as any}
        />
      )}
    </div>
  );

  // CrdtBridgeProvider: CodeCell → useCrdtBridge throws without this.
  // We provide the real WASM handle (read-only, no sync needed).
  const getHandle = useCallback(() => handleRef.current as any, []);
  const noopSync = useCallback(() => {}, []);
  const noopDirty = useCallback((_dirty: boolean) => {}, []);

  // Inner providers that need NotebookHost (PresenceProvider uses useNotebookHost)
  const innerProviders = (children: React.ReactNode) => (
    <PresenceProvider peerId="live-viewer" peerLabel="viewer" actorLabel="viewer:readonly">
      <CrdtBridgeProvider
        getHandle={getHandle}
        onSyncNeeded={noopSync}
        setDirty={noopDirty}
        localActor="viewer:readonly"
      >
        {children}
      </CrdtBridgeProvider>
    </PresenceProvider>
  );

  // Outer providers that don't need host
  const outerProviders = (children: React.ReactNode) => (
    <IsolatedRendererProvider loader={() => import("virtual:isolated-renderer")}>
      <WidgetStoreProvider>{children}</WidgetStoreProvider>
    </IsolatedRendererProvider>
  );

  if (host) {
    return (
      <NotebookHostProvider host={host}>
        {outerProviders(innerProviders(content))}
      </NotebookHostProvider>
    );
  }
  // Without host, PresenceProvider will crash (useNotebookHost).
  // Fall back without presence/crdt — NotebookView won't render CodeCell correctly.
  return outerProviders(content);
}
