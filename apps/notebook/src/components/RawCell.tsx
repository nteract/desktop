import type { EditorView, KeyBinding } from "@codemirror/view";
import { memo, type ReactNode, useCallback, useEffect, useMemo, useRef } from "react";
import { CellContainer } from "@/components/cell/CellContainer";
import { CodeMirrorEditor, type CodeMirrorEditorRef } from "@/components/editor/codemirror-editor";
import { remoteCursorsExtension } from "@/components/editor/remote-cursors";
import { searchHighlight } from "@/components/editor/search-highlight";
import { textAttributionExtension } from "@/components/editor/text-attribution";
import { usePresenceContext } from "../contexts/PresenceContext";
import { useCellKeyboardNavigation } from "../hooks/useCellKeyboardNavigation";
import { useCrdtBridge } from "../hooks/useCrdtBridge";
import {
  useIsCellFocused,
  useIsNextCellFromFocused,
  useIsPreviousCellFromFocused,
  useSearchQuery,
} from "../lib/cell-ui-state";
import { onEditorRegistered, onEditorUnregistered } from "../lib/cursor-registry";
import { detectRawFormat } from "../lib/detect-raw-format";
import { registerCellEditor, unregisterCellEditor } from "../lib/editor-registry";
import { presenceSenderExtension } from "../lib/presence-sender";
import type { RawCell as RawCellType } from "../types";
import { CellPresenceIndicators } from "./cell/CellPresenceIndicators";

interface RawCellProps {
  cell: RawCellType;
  onFocus: () => void;
  onDelete: () => void;
  onFocusPrevious?: (cursorPosition: "start" | "end") => void;
  onFocusNext?: (cursorPosition: "start" | "end") => void;
  onInsertCellAfter?: () => void;
  isLastCell?: boolean;
  /** Props for dnd-kit drag handle (applied to ribbon) */
  dragHandleProps?: Record<string, unknown>;
  /** Whether this cell is currently being dragged */
  isDragging?: boolean;
  /** Content for the right gutter (e.g., delete button) */
  rightGutterContent?: ReactNode;
}

export const RawCell = memo(function RawCell({
  cell,
  onFocus,
  onDelete,
  onFocusPrevious,
  onFocusNext,
  onInsertCellAfter,
  isLastCell = false,
  dragHandleProps,
  isDragging,
  rightGutterContent,
}: RawCellProps) {
  const isFocused = useIsCellFocused(cell.id);
  const isPreviousCellFromFocused = useIsPreviousCellFromFocused(cell.id);
  const isNextCellFromFocused = useIsNextCellFromFocused(cell.id);
  const searchQuery = useSearchQuery();
  const editorRef = useRef<CodeMirrorEditorRef>(null);
  const presence = usePresenceContext();
  const { extension: crdtBridgeExt } = useCrdtBridge(cell.id);

  // Register EditorView with the cursor registry for presence support
  const registeredViewRef = useRef<EditorView | null>(null);
  useEffect(() => {
    const tryRegister = () => {
      const view = editorRef.current?.getEditor() ?? null;
      if (view && view !== registeredViewRef.current) {
        registeredViewRef.current = view;
        registerCellEditor(cell.id, view);
        onEditorRegistered(cell.id);
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
          onEditorUnregistered(cell.id);
          unregisterCellEditor(cell.id);
          registeredViewRef.current = null;
        }
      };
    }

    return () => {
      if (registeredViewRef.current) {
        onEditorUnregistered(cell.id);
        unregisterCellEditor(cell.id);
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
  const keyMap: KeyBinding[] = useMemo(() => navigationKeyMap, [navigationKeyMap]);

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
      isNextCellFromFocused={isNextCellFromFocused}
      onFocus={onFocus}
      presenceIndicators={<CellPresenceIndicators cellId={cell.id} />}
      dragHandleProps={dragHandleProps}
      isDragging={isDragging}
      rightGutterContent={rightGutterContent}
      codeContent={
        <>
          <div className="flex items-center gap-1 py-1">
            <span className="text-xs text-muted-foreground font-mono">
              {format === "plain" ? "raw" : `raw (${format})`}
            </span>
          </div>
          <div>
            <CodeMirrorEditor
              ref={editorRef}
              initialValue={cell.source}
              language={language}
              lineWrapping
              keyMap={keyMap}
              extensions={[crdtBridgeExt, ...searchExtensions]}
              placeholder="Enter raw content..."
              className="min-h-[2rem]"
              autoFocus={isFocused}
            />
          </div>
        </>
      }
    />
  );
});
