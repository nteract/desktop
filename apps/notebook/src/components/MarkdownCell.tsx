import type { KeyBinding } from "@codemirror/view";
import { Pencil, Trash2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { CellContainer } from "@/components/cell/CellContainer";
import {
  CodeMirrorEditor,
  type CodeMirrorEditorRef,
} from "@/components/editor/codemirror-editor";
import { searchHighlight } from "@/components/editor/search-highlight";
import { IsolatedFrame, type IsolatedFrameHandle } from "@/components/isolated";
import { isDarkMode as detectDarkMode } from "@/lib/dark-mode";
import { cn } from "@/lib/utils";
import { useCellKeyboardNavigation } from "../hooks/useCellKeyboardNavigation";
import { useEditorRegistry } from "../hooks/useEditorRegistry";
import type { MarkdownCell as MarkdownCellType } from "../types";

interface MarkdownCellProps {
  cell: MarkdownCellType;
  isFocused: boolean;
  searchQuery?: string;
  onFocus: () => void;
  onUpdateSource: (source: string) => void;
  onDelete: () => void;
  onFocusPrevious?: (cursorPosition: "start" | "end") => void;
  onFocusNext?: (cursorPosition: "start" | "end") => void;
  onInsertCellAfter?: () => void;
  isLastCell?: boolean;
}

export function MarkdownCell({
  cell,
  isFocused,
  searchQuery,
  onFocus,
  onUpdateSource,
  onDelete,
  onFocusPrevious,
  onFocusNext,
  onInsertCellAfter,
  isLastCell = false,
}: MarkdownCellProps) {
  const [editing, setEditing] = useState(cell.source === "");
  const editorRef = useRef<CodeMirrorEditorRef>(null);
  const frameRef = useRef<IsolatedFrameHandle>(null);
  const viewRef = useRef<HTMLDivElement>(null);
  const { registerEditor, unregisterEditor } = useEditorRegistry();

  // Track dark mode state for iframe theme sync
  const [darkMode, setDarkMode] = useState(() => detectDarkMode());

  useEffect(() => {
    const observer = new MutationObserver(() => {
      setDarkMode(detectDarkMode());
    });
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["class", "data-theme", "data-mode"],
    });
    return () => observer.disconnect();
  }, []);

  // Register editor with the registry for cross-cell navigation
  useEffect(() => {
    if (editing && editorRef.current) {
      registerEditor(cell.id, {
        focus: () => editorRef.current?.focus(),
        setCursorPosition: (position) =>
          editorRef.current?.setCursorPosition(position),
      });
    }
    return () => unregisterEditor(cell.id);
  }, [cell.id, editing, registerEditor, unregisterEditor]);

  const handleDoubleClick = useCallback(() => {
    setEditing(true);
  }, []);

  const handleBlur = useCallback(() => {
    if (cell.source.trim()) {
      setEditing(false);
    }
  }, [cell.source]);

  // Render markdown content when iframe is ready
  const handleFrameReady = useCallback(() => {
    if (!frameRef.current || !cell.source) return;
    frameRef.current.render({
      mimeType: "text/markdown",
      data: cell.source,
      cellId: cell.id,
      replace: true,
    });
  }, [cell.source, cell.id]);

  // Sync markdown to iframe whenever source changes (supports RTC updates)
  useEffect(() => {
    if (frameRef.current?.isReady && cell.source) {
      frameRef.current.render({
        mimeType: "text/markdown",
        data: cell.source,
        cellId: cell.id,
        replace: true,
      });
    }
  }, [cell.source, cell.id]);

  // Handle link clicks from iframe
  const handleLinkClick = useCallback((url: string) => {
    window.open(url, "_blank", "noopener,noreferrer");
  }, []);

  // Handle keyboard navigation in view mode (when not editing)
  const handleViewKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "ArrowDown") {
        onFocusNext?.("start");
        e.preventDefault();
      } else if (e.key === "ArrowUp") {
        onFocusPrevious?.("end");
        e.preventDefault();
      } else if (e.key === "Enter" && e.shiftKey) {
        // Shift+Enter: move to next cell (like execute for code cells)
        onFocusNext?.("start");
        e.preventDefault();
      } else if (e.key === "Enter" && !e.shiftKey) {
        // Enter: enter edit mode
        setEditing(true);
        e.preventDefault();
      }
    },
    [onFocusNext, onFocusPrevious],
  );

  // Handle focus next, creating a new cell if at the end
  const handleFocusNextOrCreate = useCallback(
    (cursorPosition: "start" | "end") => {
      // For markdown, close edit mode first
      if (cell.source.trim()) {
        setEditing(false);
      }
      if (isLastCell && onInsertCellAfter) {
        onInsertCellAfter();
      } else if (onFocusNext) {
        onFocusNext(cursorPosition);
      }
    },
    [cell.source, isLastCell, onFocusNext, onInsertCellAfter],
  );

  // Search highlight extension for edit mode
  const searchExtensions = useMemo(
    () => searchHighlight(searchQuery || ""),
    [searchQuery],
  );

  // Get keyboard navigation bindings
  const navigationKeyMap = useCellKeyboardNavigation({
    onFocusPrevious: onFocusPrevious ?? (() => {}),
    onFocusNext: handleFocusNextOrCreate,
    onExecute: () => {}, // No-op for markdown, enables Shift+Enter navigation
    onDelete,
  });

  // Combine navigation with markdown-specific keys
  const keyMap: KeyBinding[] = useMemo(
    () => [
      ...navigationKeyMap,
      {
        key: "Escape",
        run: () => {
          if (cell.source.trim()) {
            setEditing(false);
          }
          return true;
        },
      },
    ],
    [navigationKeyMap, cell.source],
  );

  // Focus editor when entering edit mode (after initial mount)
  const initialMountRef = useRef(true);
  useEffect(() => {
    if (initialMountRef.current) {
      initialMountRef.current = false;
      return;
    }
    if (editing) {
      requestAnimationFrame(() => {
        editorRef.current?.focus();
      });
    }
  }, [editing]);

  // Forward search query to the markdown iframe
  useEffect(() => {
    if (!editing && frameRef.current?.isReady) {
      frameRef.current.search(searchQuery || "");
    }
  }, [searchQuery, editing]);

  // Focus view section when cell becomes focused but not editing
  useEffect(() => {
    if (isFocused && !editing) {
      requestAnimationFrame(() => {
        viewRef.current?.focus();
      });
    }
  }, [isFocused, editing]);

  return (
    <CellContainer
      id={cell.id}
      cellType="markdown"
      isFocused={isFocused}
      onFocus={onFocus}
    >
      {/* Editor section - hidden when not editing */}
      <div className={editing ? "block" : "hidden"}>
        <div className="flex items-center gap-1 py-1">
          <span className="text-xs text-muted-foreground font-mono">md</span>
          <div className="flex-1" />
          <div className="cell-controls opacity-0 group-hover:opacity-100 transition-opacity">
            <button
              type="button"
              tabIndex={-1}
              onClick={onDelete}
              className="flex items-center justify-center rounded p-1 text-muted-foreground/40 transition-colors hover:text-destructive"
              title="Delete cell"
            >
              <Trash2 className="h-3.5 w-3.5" />
            </button>
          </div>
        </div>
        <div>
          <CodeMirrorEditor
            ref={editorRef}
            value={cell.source}
            language="markdown"
            lineWrapping
            onValueChange={onUpdateSource}
            onBlur={handleBlur}
            keyMap={keyMap}
            extensions={searchExtensions}
            placeholder="Enter markdown..."
            className="min-h-[2rem]"
            autoFocus={editing}
          />
        </div>
      </div>

      {/* View section - hidden when editing */}
      <div
        ref={viewRef}
        role="textbox"
        aria-readonly
        aria-label="Markdown cell content"
        tabIndex={0}
        className={cn(
          "py-2 cursor-text relative group/md outline-none",
          editing && "hidden",
        )}
        onDoubleClick={handleDoubleClick}
        onKeyDown={handleViewKeyDown}
      >
        {cell.source ? (
          <IsolatedFrame
            ref={frameRef}
            darkMode={darkMode}
            minHeight={24}
            maxHeight={2000}
            onReady={handleFrameReady}
            onLinkClick={handleLinkClick}
            onDoubleClick={handleDoubleClick}
            onError={(err) =>
              console.error("[MarkdownCell] iframe error:", err)
            }
            className="w-full"
          />
        ) : (
          <p className="text-muted-foreground italic">Double-click to edit</p>
        )}
        <button
          type="button"
          onClick={() => setEditing(true)}
          className="absolute top-2 right-2 opacity-0 group-hover/md:opacity-100 rounded p-1 text-muted-foreground transition-opacity hover:text-foreground"
          title="Edit"
        >
          <Pencil className="h-3.5 w-3.5" />
        </button>
      </div>
    </CellContainer>
  );
}
