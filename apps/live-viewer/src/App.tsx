import { BrowserRouter, Routes, Route, useNavigate, useParams } from "react-router";
import { useEffect, useState } from "react";
import { NotebookViewer } from "./components/NotebookViewer";

interface RoomInfo {
  notebook_id: string;
  active_peers: number;
  has_kernel: boolean;
  kernel_type?: string;
  kernel_status?: string;
  env_source?: string;
}

export function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route path="/" element={<NotebookList />} />
        <Route path="/:notebookId" element={<ViewerPage />} />
      </Routes>
    </BrowserRouter>
  );
}

function NotebookList() {
  const [rooms, setRooms] = useState<RoomInfo[]>([]);
  const [error, setError] = useState<string | null>(null);
  const navigate = useNavigate();

  useEffect(() => {
    async function fetchRooms() {
      try {
        const resp = await fetch("/api/notebooks");
        const data: RoomInfo[] = await resp.json();
        setRooms(data);
        setError(null);
      } catch (e) {
        setError(String(e));
      }
    }
    fetchRooms();
    const interval = setInterval(fetchRooms, 5000);
    return () => clearInterval(interval);
  }, []);

  return (
    <div className="dark min-h-screen bg-background text-foreground">
      <header className="sticky top-0 z-50 flex items-center gap-3 border-b border-border bg-background/80 px-6 py-3 backdrop-blur-sm">
        <h1 className="text-sm font-semibold">nteract live viewer</h1>
        <span className="rounded-full bg-secondary px-2 py-0.5 text-xs text-muted-foreground">
          {rooms.length} notebooks
        </span>
      </header>

      <main className="mx-auto max-w-3xl p-6">
        {error && (
          <div className="mb-4 rounded-md border border-destructive/30 bg-destructive/10 p-3 text-sm text-destructive-foreground">
            {error}
          </div>
        )}

        {rooms.length === 0 && !error && (
          <div className="py-20 text-center text-muted-foreground">
            <p className="text-lg">No active notebooks</p>
            <p className="mt-1 text-sm opacity-70">Open a notebook in nteract to see it here.</p>
          </div>
        )}

        <div className="flex flex-col gap-2">
          {rooms.map((room) => (
            <button
              key={room.notebook_id}
              onClick={() => navigate(`/${room.notebook_id}`)}
              className="flex items-center gap-3 rounded-lg border border-border bg-card p-4 text-left transition-colors hover:bg-secondary"
            >
              <div className="flex-1">
                <div className="font-mono text-sm text-card-foreground">
                  {room.notebook_id.slice(0, 8)}
                </div>
                <div className="mt-0.5 text-xs text-muted-foreground">
                  {room.active_peers} peer{room.active_peers !== 1 ? "s" : ""}
                  {room.kernel_type && ` · ${room.kernel_type}`}
                  {room.env_source && ` · ${room.env_source}`}
                </div>
              </div>
              {room.kernel_status && (
                <span
                  className={`rounded-full px-2 py-0.5 text-xs font-medium ${
                    room.kernel_status === "idle"
                      ? "bg-green-900/30 text-green-400"
                      : room.kernel_status === "busy"
                        ? "bg-yellow-900/30 text-yellow-400"
                        : "bg-blue-900/30 text-blue-400"
                  }`}
                >
                  {room.kernel_status}
                </span>
              )}
            </button>
          ))}
        </div>
      </main>
    </div>
  );
}

function ViewerPage() {
  const { notebookId } = useParams<{ notebookId: string }>();
  const navigate = useNavigate();

  if (!notebookId) return null;

  return (
    <div className="dark min-h-screen bg-background text-foreground">
      <header className="sticky top-0 z-50 flex items-center gap-3 border-b border-border bg-background/80 px-6 py-3 backdrop-blur-sm">
        <button
          onClick={() => navigate("/")}
          className="rounded-md border border-border px-3 py-1 text-xs text-muted-foreground hover:bg-secondary"
        >
          &larr; Back
        </button>
        <h1 className="text-sm font-semibold">nteract live viewer</h1>
        <span className="ml-auto font-mono text-xs text-muted-foreground">
          {notebookId.slice(0, 8)}
        </span>
      </header>
      <NotebookViewer notebookId={notebookId} />
    </div>
  );
}
