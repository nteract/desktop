import { useEffect, useRef, useState } from "react";
import { SyncEngine } from "runtimed/src/sync-engine";
import type { SyncableHandle } from "runtimed/src/handle";
import type { RuntimeState, ExecutionState, QueueEntry } from "runtimed/src/runtime-state";
import type { JupyterOutput } from "@/components/cell/jupyter-output";
import init, { NotebookHandle } from "runtimed-wasm/runtimed_wasm.js";
import { WebSocketTransport } from "~/lib/ws-transport";
import { CellView } from "./CellView";

interface CellData {
  id: string;
  cell_type: string;
  source: string;
  execution_count: number | null;
  outputs: JupyterOutput[];
}

interface Props {
  notebookId: string;
}

export function NotebookViewer({ notebookId }: Props) {
  const [cells, setCells] = useState<CellData[]>([]);
  const [status, setStatus] = useState<"connecting" | "syncing" | "live" | "error">("connecting");
  const [kernelStatus, setKernelStatus] = useState<string>("");
  const [executions, setExecutions] = useState<Record<string, ExecutionState>>({});
  const [queue, setQueue] = useState<{ executing: QueueEntry | null; queued: QueueEntry[] }>({
    executing: null,
    queued: [],
  });
  const engineRef = useRef<SyncEngine | null>(null);
  const transportRef = useRef<WebSocketTransport | null>(null);
  const handleRef = useRef<SyncableHandle | null>(null);

  useEffect(() => {
    let disposed = false;

    async function initViewer() {
      await init();

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
        setExecutions(state.executions);
        setQueue(state.queue);
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

    initViewer();

    return () => {
      disposed = true;
      engineRef.current?.stop();
      transportRef.current?.disconnect();
      const h = handleRef.current as unknown as { free?(): void } | null;
      h?.free?.();
    };
  }, [notebookId]);

  const cellExecutionStatus = (cellId: string): ExecutionState | null => {
    for (const exec of Object.values(executions)) {
      if (exec.cell_id === cellId && (exec.status === "queued" || exec.status === "running")) {
        return exec;
      }
    }
    return null;
  };

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
            <KernelStatusBadge status={kernelStatus} />
          </>
        )}
        <span className="text-border">·</span>
        <span>{cells.length} cells</span>
        {(queue.executing || queue.queued.length > 0) && (
          <>
            <span className="text-border">·</span>
            <QueueIndicator executing={queue.executing} queued={queue.queued} />
          </>
        )}
      </div>

      {cells.length === 0 && status === "live" && (
        <div className="py-12 text-center text-muted-foreground">Empty notebook</div>
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
          <CellView key={cell.id} cell={cell} executionState={cellExecutionStatus(cell.id)} />
        ))}
      </div>
    </div>
  );
}

function KernelStatusBadge({ status }: { status: string }) {
  const colors: Record<string, string> = {
    idle: "text-green-400",
    busy: "text-amber-400",
    starting: "text-blue-400",
    error: "text-red-400",
    not_started: "text-muted-foreground",
    shutdown: "text-muted-foreground",
  };
  const icons: Record<string, string> = {
    idle: "\u25CF",
    busy: "\u25CF",
    starting: "\u25CB",
    error: "\u2716",
  };
  return (
    <span className={colors[status] ?? "text-muted-foreground"}>
      {icons[status] && <span className="mr-0.5">{icons[status]}</span>}
      kernel: {status}
    </span>
  );
}

function QueueIndicator({
  executing,
  queued,
}: {
  executing: QueueEntry | null;
  queued: QueueEntry[];
}) {
  const total = (executing ? 1 : 0) + queued.length;
  return <span className="text-amber-400 animate-pulse">{total} in queue</span>;
}
