import { BrowserRouter, Routes, Route, Link, useNavigate, useParams } from "react-router";
import { FullTreeViewer } from "./components/FullTreeViewer";
import { useEffect, useState } from "react";
import { NotebookViewer } from "./components/NotebookViewer";

interface RoomInfo {
  notebook_id: string;
  active_peers: number;
  has_kernel: boolean;
  kernel_type?: string;
  kernel_status?: string;
  env_source?: string;
  ephemeral?: boolean;
}

export function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route path="/" element={<NotebookList />} />
        <Route path="/:notebookId" element={<FullTreePage />} />
      </Routes>
    </BrowserRouter>
  );
}

function NotebookList() {
  const [rooms, setRooms] = useState<RoomInfo[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [lastFetch, setLastFetch] = useState<Date | null>(null);
  const navigate = useNavigate();

  useEffect(() => {
    async function fetchRooms() {
      try {
        const resp = await fetch("/api/notebooks");
        const data: RoomInfo[] = await resp.json();
        setRooms(data);
        setError(null);
        setLastFetch(new Date());
      } catch (e) {
        setError(String(e));
      }
    }
    fetchRooms();
    const interval = setInterval(fetchRooms, 3000);
    return () => clearInterval(interval);
  }, []);

  return (
    <div className="dark min-h-screen bg-background text-foreground">
      <header className="sticky top-0 z-50 flex items-center gap-3 border-b border-border bg-background/80 px-6 py-3 backdrop-blur-sm">
        <h1 className="text-sm font-semibold">nteract live viewer</h1>
        <span className="rounded-full bg-secondary px-2 py-0.5 text-xs text-muted-foreground">
          {rooms.length} notebook{rooms.length !== 1 ? "s" : ""}
        </span>
        {lastFetch && (
          <span className="ml-auto text-[10px] text-muted-foreground/60">polling every 3s</span>
        )}
      </header>

      <main className="mx-auto max-w-3xl p-6">
        {error && (
          <div className="mb-4 rounded-md border border-red-500/30 bg-red-950/20 p-3 text-sm text-red-400">
            Failed to reach daemon: {error}
          </div>
        )}

        {rooms.length === 0 && !error && (
          <div className="py-20 text-center text-muted-foreground">
            <p className="text-lg">No active notebooks</p>
            <p className="mt-2 text-sm opacity-70">Open a notebook in nteract to see it here.</p>
          </div>
        )}

        <div className="flex flex-col gap-2">
          {rooms.map((room) => (
            <button
              key={room.notebook_id}
              onClick={() => navigate(`/${room.notebook_id}`)}
              className="group flex items-center gap-4 rounded-lg border border-border bg-card p-4 text-left transition-all hover:border-muted-foreground/30 hover:bg-secondary"
            >
              <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-secondary text-muted-foreground">
                {room.ephemeral ? (
                  <span className="text-base">*</span>
                ) : (
                  <span className="text-xs font-mono">.nb</span>
                )}
              </div>
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <span className="font-mono text-sm text-card-foreground">
                    {room.notebook_id.slice(0, 8)}
                  </span>
                  {room.ephemeral && (
                    <span className="rounded bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">
                      ephemeral
                    </span>
                  )}
                </div>
                <div className="mt-0.5 flex items-center gap-1.5 text-xs text-muted-foreground">
                  <span>
                    {room.active_peers} peer{room.active_peers !== 1 ? "s" : ""}
                  </span>
                  {room.kernel_type && (
                    <>
                      <span className="text-border">·</span>
                      <span>{room.kernel_type}</span>
                    </>
                  )}
                  {room.env_source && (
                    <>
                      <span className="text-border">·</span>
                      <span>{room.env_source}</span>
                    </>
                  )}
                </div>
              </div>
              {room.kernel_status && <KernelPill status={room.kernel_status} />}
              <span className="text-muted-foreground/40 transition-transform group-hover:translate-x-0.5">
                &rsaquo;
              </span>
            </button>
          ))}
        </div>
      </main>
    </div>
  );
}

function KernelPill({ status }: { status: string }) {
  const styles: Record<string, string> = {
    idle: "bg-green-900/30 text-green-400",
    busy: "bg-amber-900/30 text-amber-400",
    starting: "bg-blue-900/30 text-blue-400",
    error: "bg-red-900/30 text-red-400",
  };
  return (
    <span
      className={`rounded-full px-2 py-0.5 text-xs font-medium ${styles[status] ?? "bg-secondary text-muted-foreground"}`}
    >
      {status}
    </span>
  );
}

function ViewerPage() {
  const { notebookId } = useParams<{ notebookId: string }>();

  if (!notebookId) return null;

  return (
    <div className="dark min-h-screen bg-background text-foreground">
      <header className="sticky top-0 z-50 flex items-center gap-3 border-b border-border bg-background/80 px-6 py-3 backdrop-blur-sm">
        <Link
          to="/"
          className="rounded-md border border-border px-3 py-1 text-xs text-muted-foreground hover:bg-secondary"
        >
          &larr; Back
        </Link>
        <h1 className="text-sm font-semibold">nteract live viewer</h1>
        <span className="ml-auto font-mono text-xs text-muted-foreground">
          {notebookId.slice(0, 8)}
        </span>
      </header>
      <NotebookViewer notebookId={notebookId} />
    </div>
  );
}

function FullTreePage() {
  const { notebookId } = useParams<{ notebookId: string }>();

  if (!notebookId) return null;

  return (
    <div className="dark min-h-screen bg-background text-foreground">
      <header className="sticky top-0 z-50 flex items-center gap-3 border-b border-border bg-background/80 px-6 py-3 backdrop-blur-sm">
        <Link
          to="/"
          className="rounded-md border border-border px-3 py-1 text-xs text-muted-foreground hover:bg-secondary"
        >
          &larr; Back
        </Link>
        <h1 className="text-sm font-semibold">FULL TREE TEST</h1>
        <span className="ml-auto font-mono text-xs text-muted-foreground">
          {notebookId.slice(0, 8)}
        </span>
      </header>
      <FullTreeViewer notebookId={notebookId} />
    </div>
  );
}
