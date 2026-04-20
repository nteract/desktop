import { useEffect, useRef, useState } from "react";
import { SyncEngine } from "runtimed/src/sync-engine";
import type { SyncableHandle } from "runtimed/src/handle";
import { WebSocketTransport } from "~/lib/ws-transport";
import { CellView } from "./CellView";

interface CellData {
  id: string;
  cell_type: string;
  source: string;
  execution_count: number | null;
  outputs: OutputData[];
}

interface OutputData {
  output_type: string;
  text?: string;
  data?: Record<string, unknown>;
  name?: string;
  ename?: string;
  evalue?: string;
  traceback?: string[];
}

interface Props {
  notebookId: string;
}

export function NotebookViewer({ notebookId }: Props) {
  const [cells, setCells] = useState<CellData[]>([]);
  const [status, setStatus] = useState<"connecting" | "syncing" | "live" | "error">("connecting");
  const [kernelStatus, setKernelStatus] = useState<string>("");
  const engineRef = useRef<SyncEngine | null>(null);
  const transportRef = useRef<WebSocketTransport | null>(null);
  const handleRef = useRef<SyncableHandle | null>(null);

  useEffect(() => {
    let disposed = false;

    async function init() {
      const { default: wasmInit, NotebookHandle } = await import("runtimed-wasm");
      await wasmInit();

      if (disposed) return;

      const handle = NotebookHandle.create_bootstrap("live-viewer");
      handleRef.current = handle as unknown as SyncableHandle;

      const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
      const url = `${proto}//${window.location.host}/ws/join?id=${encodeURIComponent(notebookId)}`;

      const transport = new WebSocketTransport({
        url,
        onOpen: () => setStatus("syncing"),
        onClose: () => {
          if (!disposed) setStatus("error");
        },
      });
      transportRef.current = transport;

      const engine = new SyncEngine({
        getHandle: () => handleRef.current,
        transport,
        logger: {
          debug: () => {},
          info: console.info.bind(console),
          warn: console.warn.bind(console),
          error: console.error.bind(console),
        },
      });
      engineRef.current = engine;

      engine.cellChanges$.subscribe((changeset) => {
        if (disposed) return;
        materializeCells();
        if (status !== "live") setStatus("live");
      });

      engine.initialSyncComplete$.subscribe(() => {
        if (disposed) return;
        setStatus("live");
        materializeCells();
      });

      engine.runtimeState$.subscribe((state: unknown) => {
        if (disposed) return;
        const rs = state as { kernel?: { status?: string } } | null;
        if (rs?.kernel?.status) {
          setKernelStatus(rs.kernel.status);
        }
      });

      engine.start();
    }

    function materializeCells() {
      const handle = handleRef.current;
      if (!handle) return;

      try {
        const h = handle as unknown as { get_cells_json(): string };
        const json = h.get_cells_json();
        const parsed = JSON.parse(json) as CellData[];
        setCells(parsed);
      } catch {
        // Handle may not have synced yet
      }
    }

    init();

    return () => {
      disposed = true;
      engineRef.current?.stop();
      transportRef.current?.disconnect();
      const h = handleRef.current as unknown as { free?(): void } | null;
      h?.free?.();
    };
  }, [notebookId]);

  return (
    <div className="mx-auto max-w-4xl px-4 py-4">
      <div className="mb-3 flex items-center gap-2 text-xs text-muted-foreground">
        <span
          className={`h-2 w-2 rounded-full ${
            status === "live"
              ? "bg-green-500"
              : status === "syncing"
                ? "bg-blue-500 animate-pulse"
                : status === "error"
                  ? "bg-red-500"
                  : "bg-gray-500"
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
        <span>{cells.length} cells</span>
      </div>

      {cells.length === 0 && status === "live" && (
        <div className="py-12 text-center text-muted-foreground">
          Empty notebook
        </div>
      )}

      {cells.length === 0 && status !== "live" && (
        <div className="py-12 text-center text-muted-foreground">
          {status === "connecting" && "Connecting to relay..."}
          {status === "syncing" && "Syncing notebook..."}
          {status === "error" && "Connection lost"}
        </div>
      )}

      <div className="flex flex-col gap-0">
        {cells.map((cell) => (
          <CellView key={cell.id} cell={cell} />
        ))}
      </div>
    </div>
  );
}
