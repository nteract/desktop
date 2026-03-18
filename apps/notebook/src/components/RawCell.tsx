import type { EditorView, KeyBinding } from "@codemirror/view";
import { Trash2 } from "lucide-react";
import { memo, useCallback, useEffect, useMemo, useRef } from "react";
import { CellContainer } from "@/components/cell/CellContainer";
import {
  CodeMirrorEditor,
  type CodeMirrorEditorRef,
} from "@/components/editor/codemirror-editor";
import { remoteCursorsExtension } from "@/components/editor/remote-cursors";
import { searchHighlight } from "@/components/editor/search-highlight";
import { textAttributionExtension } from "@/components/editor/text-attribution";
import { usePresenceContext } from "../contexts/PresenceContext";
import { useCellKeyboardNavigation } from "../hooks/useCellKeyboardNavigation";
import {
  registerAttributionEditor,
  unregisterAttributionEditor,
} from "../lib/attribution-registry";
import { registerEditor, unregisterEditor } from "../lib/cursor-registry";
import { detectRawFormat } from "../lib/detect-raw-format";
import { presenceSenderExtension } from "../lib/presence-sender";
import type { RawCell as RawCellType } from "../types";
import { CellPresenceIndicators } from "./cell/CellPresenceIndicators";

interface RawCellProps {
  cell: RawCellType;
  isFocused: boolean;
  searchQuery?: string;
  onFocus: () => void;
  onUpdateSource: (source: string) => void;
  onDelete: () => void;
  onFocusPrevious?: (cursorPosition: "start" | "end") => void;
  onFocusNext?: (cursorPosition: "start" | "end") => void;
  onInsertCellAfter?: () => void;
  isLastCell?: boolean;
  isPreviousCellFromFocused?: boolean;
  dragHandleProps?: Record<string, unknown>;
  isDragging?: boolean;
}

export const RawCell = memo(function RawCell({
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
  isPreviousCellFromFocused,
  dragHandleProps,
  isDragging,
}: RawCellProps) {
  const editorRef = useRef<CodeMirrorEditorRef>(null);
  const presence = usePresenceContext();

  // Register EditorView with the cursor registry for presence support
  const registeredViewRef = useRef<EditorView | null>(null);
  useEffect(() => {
    const tryRegister = () => {
      const view = editorRef.current?.getEditor() ?? null;
      if (view && view !== registeredViewRef.current) {
        registeredViewRef.current = view;
        registerEditor(cell.id, view);
        registerAttributionEditor(cell.id, view);
        return true;
      }
      return false;
    };

    if (!tryRegister()) {
      let attempts = 0;
      const intervalId = window.setInterval(() => {
        attempts += 1;
        if (tryRegister() || attempts >= 40) {
          clearInterval(intervalId);
        }
      }, 50);

      return () => {
        clearInterval(intervalId);
        if (registeredViewRef.current) {
          unregisterEditor(cell.id);
          unregisterAttributionEditor(cell.id);
          registeredViewRef.current = null;
        }
      };
    }

    return () => {
      if (registeredViewRef.current) {
        unregisterEditor(cell.id);
        unregisterAttributionEditor(cell.id);
        registeredViewRef.current = null;
      }
    };
  }, [cell.id]);

  // Detect format for syntax highlighting
  const format = useMemo(
    () => detectRawFormat(cell.source, cell.metadata),
    [cell.source, cell.metadata],
  );

  // Map format to CodeMirror language
  const language = useMemo(() => {
    switch (format) {
      case "yaml":
        return "yaml";
      case "toml":
        return "toml";
      case "json":
        return "json";
      default:
        return "plain";
    }
  }, [format]);

  // Handle focus next, creating a new cell if at the end
  const handleFocusNextOrCreate = useCallback(
    (cursorPosition: "start" | "end") => {
      if (isLastCell && onInsertCellAfter) {
        onInsertCellAfter();
      } else if (onFocusNext) {
        onFocusNext(cursorPosition);
      }
    },
    [isLastCell, onFocusNext, onInsertCellAfter],
  );

  // Remote cursors extension (stable — no deps that change)
  const remoteCursorsExt = useMemo(() => remoteCursorsExtension(), []);

  // Text attribution extension (stable — no deps that change)
  const textAttributionExt = useMemo(() => textAttributionExtension(), []);

  // Presence sender extension — broadcasts local cursor/selection to other peers
  const presenceSenderExt = useMemo(() => {
    if (!presence) return [];
    return [
      presenceSenderExtension(cell.id, {
        onCursor: presence.setCursor,
        onSelection: presence.setSelection,
      }),
    ];
  }, [cell.id, presence]);

  // Search highlight extension + remote cursors + presence sender
  const searchExtensions = useMemo(
    () => [
      ...searchHighlight(searchQuery || ""),
      ...remoteCursorsExt,
      ...textAttributionExt,
      ...presenceSenderExt,
    ],
    [searchQuery, remoteCursorsExt, textAttributionExt, presenceSenderExt],
  );

  // Get keyboard navigation bindings
  const navigationKeyMap = useCellKeyboardNavigation({
    onFocusPrevious: onFocusPrevious ?? (() => {}),
    onFocusNext: handleFocusNextOrCreate,
    onExecute: () => {}, // No-op for raw cells, enables Shift+Enter navigation
    onDelete,
    cellId: cell.id,
  });

  // Use navigation key bindings directly (Shift+Enter already handled by useCellKeyboardNavigation)
  const keyMap: KeyBinding[] = useMemo(
    () => navigationKeyMap,
    [navigationKeyMap],
  );

  // Focus editor when cell becomes focused
  useEffect(() => {
    if (isFocused) {
      requestAnimationFrame(() => {
        editorRef.current?.focus();
      });
    }
  }, [isFocused]);

  return (
    <CellContainer
      id={cell.id}
      cellType="raw"
      isFocused={isFocused}
      isPreviousCellFromFocused={isPreviousCellFromFocused}
      onFocus={onFocus}
      presenceIndicators={<CellPresenceIndicators cellId={cell.id} />}
      dragHandleProps={dragHandleProps}
      isDragging={isDragging}
    >
      <div className="flex items-center gap-1 py-1">
        <span className="text-xs text-muted-foreground font-mono">
          {format === "plain" ? "raw" : `raw (${format})`}
        </span>
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
          language={language}
          lineWrapping
          onValueChange={onUpdateSource}
          keyMap={keyMap}
          extensions={searchExtensions}
          placeholder="Enter raw content..."
          className="min-h-[2rem]"
          autoFocus={isFocused}
        />
      </div>
    </CellContainer>
  );
});
