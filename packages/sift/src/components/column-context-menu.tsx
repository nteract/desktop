/**
 * Column header context menu.
 * Positioned manually at click coordinates, rendered as a portal.
 */
import { useEffect, useRef } from "react";
import type { ColumnType } from "../table";

export type ColumnAction =
  | { kind: "sort"; direction: "asc" | "desc" }
  | { kind: "pin" }
  | { kind: "unpin" }
  | { kind: "cast"; targetType: ColumnType }
  | { kind: "undo-cast" };

export type ColumnMenuState = {
  colIndex: number;
  colName: string;
  colType: ColumnType;
  isPinned: boolean;
  isCast: boolean;
  isStreaming: boolean;
  sortDirection: "asc" | "desc" | null;
  x: number;
  y: number;
} | null;

type Props = {
  state: ColumnMenuState;
  onAction: (colIndex: number, action: ColumnAction) => void;
  onClose: () => void;
};

export function ColumnContextMenu({ state, onAction, onClose }: Props) {
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!state) return;
    function onClick(e: MouseEvent) {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        onClose();
      }
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    const timer = setTimeout(() => {
      document.addEventListener("mousedown", onClick, true);
      document.addEventListener("keydown", onKey, true);
    }, 0);
    return () => {
      clearTimeout(timer);
      document.removeEventListener("mousedown", onClick, true);
      document.removeEventListener("keydown", onKey, true);
    };
  }, [state, onClose]);

  if (!state) return null;

  const { colIndex, colName, colType, isPinned, isCast, isStreaming, sortDirection, x, y } = state;

  function act(action: ColumnAction) {
    onAction(colIndex, action);
    onClose();
  }

  return (
    <div
      ref={menuRef}
      className="fixed z-50 min-w-[12rem] overflow-hidden rounded-lg border border-[var(--rule)] bg-[var(--panel)] p-1 shadow-lg"
      style={{ left: x, top: y }}
    >
      <div className="px-2 py-1.5 text-xs font-semibold text-[var(--muted)] uppercase tracking-wider">
        {colName}
      </div>
      <div className="h-px bg-[var(--rule)] -mx-1 my-1" />

      <MenuItem onClick={() => act({ kind: "sort", direction: "asc" })}>
        Sort ascending {sortDirection === "asc" && <Check />}
      </MenuItem>
      <MenuItem onClick={() => act({ kind: "sort", direction: "desc" })}>
        Sort descending {sortDirection === "desc" && <Check />}
      </MenuItem>

      <div className="h-px bg-[var(--rule)] -mx-1 my-1" />

      {isPinned ? (
        <MenuItem onClick={() => act({ kind: "unpin" })}>Unpin column</MenuItem>
      ) : (
        <MenuItem onClick={() => act({ kind: "pin" })}>Pin column</MenuItem>
      )}

      <div className="h-px bg-[var(--rule)] -mx-1 my-1" />

      {isStreaming ? (
        <div className="px-2 py-1.5 text-xs text-[var(--muted)] italic">
          Some operations hidden while loading
        </div>
      ) : (
        <>
          <div className="px-2 py-1 text-xs text-[var(--muted)]">Treat as…</div>
          <MenuItem onClick={() => act({ kind: "cast", targetType: "categorical" })}>
            Text {colType === "categorical" && <Check />}
          </MenuItem>
          <MenuItem onClick={() => act({ kind: "cast", targetType: "numeric" })}>
            Number {colType === "numeric" && <Check />}
          </MenuItem>
          <MenuItem onClick={() => act({ kind: "cast", targetType: "timestamp" })}>
            Date {colType === "timestamp" && <Check />}
          </MenuItem>
          <MenuItem onClick={() => act({ kind: "cast", targetType: "boolean" })}>
            Boolean {colType === "boolean" && <Check />}
          </MenuItem>

          {isCast && (
            <>
              <div className="h-px bg-[var(--rule)] -mx-1 my-1" />
              <MenuItem onClick={() => act({ kind: "undo-cast" })}>
                Revert to original type
              </MenuItem>
            </>
          )}
        </>
      )}
    </div>
  );
}

function MenuItem({ children, onClick }: { children: React.ReactNode; onClick: () => void }) {
  return (
    <button
      className="flex w-full items-center justify-between rounded-sm px-2 py-1.5 text-sm text-[var(--ink)] hover:bg-[color-mix(in_srgb,var(--accent)_8%,transparent)] cursor-default outline-none"
      onClick={onClick}
    >
      {children}
    </button>
  );
}

function Check() {
  return <span className="text-[var(--accent)] text-xs">✓</span>;
}
