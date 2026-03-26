import type { EditorView, KeyBinding } from "@codemirror/view";
import { Pencil } from "lucide-react";
import {
  memo,
  type ReactNode,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { CellContainer } from "@/components/cell/CellContainer";
import {
  CodeMirrorEditor,
  type CodeMirrorEditorRef,
} from "@/components/editor/codemirror-editor";
import { remoteCursorsExtension } from "@/components/editor/remote-cursors";
import { searchHighlight } from "@/components/editor/search-highlight";
import { textAttributionExtension } from "@/components/editor/text-attribution";
import { IsolatedFrame, type IsolatedFrameHandle } from "@/components/isolated";
import { useDarkMode } from "@/lib/dark-mode";
import { cn } from "@/lib/utils";
import { usePresenceContext } from "../contexts/PresenceContext";
import { useCellKeyboardNavigation } from "../hooks/useCellKeyboardNavigation";
import { useCrdtBridge } from "../hooks/useCrdtBridge";
import {
  registerAttributionEditor,
  unregisterAttributionEditor,
} from "../lib/attribution-registry";
import { useBlobPort } from "../lib/blob-port";
import { registerEditor, unregisterEditor } from "../lib/cursor-registry";
import { logger } from "../lib/logger";
import { rewriteMarkdownAssetRefs } from "../lib/markdown-assets";
import { openUrl } from "../lib/open-url";
import { presenceSenderExtension } from "../lib/presence-sender";
import type { MarkdownCell as MarkdownCellType } from "../types";
import { CellPresenceIndicators } from "./cell/CellPresenceIndicators";

const handleIframeError = (err: { message: string; stack?: string }) =>
  logger.error("[MarkdownCell] iframe error:", err);

interface MarkdownCellProps {
  cell: MarkdownCellType;
  isFocused: boolean;
  searchQuery?: string;
  onFocus: () => void;
  onDelete: () => void;
  onFocusPrevious?: (cursorPosition: "start" | "end") => void;
  onFocusNext?: (cursorPosition: "start" | "end") => void;
  onInsertCellAfter?: () => void;
  isLastCell?: boolean;
  /** Whether this cell is immediately before the focused cell */
  isPreviousCellFromFocused?: boolean;
  /** Whether this cell is immediately after the focused cell */
  isNextCellFromFocused?: boolean;
  /** Props for dnd-kit drag handle (applied to ribbon) */
  dragHandleProps?: Record<string, unknown>;
  /** Whether this cell is currently being dragged */
  isDragging?: boolean;
  /** Content for the right gutter (e.g., delete button) */
  rightGutterContent?: ReactNode;
}

export const MarkdownCell = memo(function MarkdownCell({
  cell,
  isFocused,
  searchQuery,
  onFocus,
  onDelete,
  onFocusPrevious,
  onFocusNext,
  onInsertCellAfter,
  isLastCell = false,
  isPreviousCellFromFocused,
  isNextCellFromFocused,
  dragHandleProps,
  isDragging,
  rightGutterContent,
}: MarkdownCellProps) {
  const applyInlineFormatting = useCallback(
    (prefix: string, suffix = prefix) =>
      (view: EditorView) => {
        const selection = view.state.selection.main;
        const selectedText = view.state.doc.sliceString(
          selection.from,
          selection.to,
        );
        const wrappedText = `${prefix}${selectedText}${suffix}`;

        view.dispatch({
          changes: {
            from: selection.from,
            to: selection.to,
            insert: wrappedText,
          },
          selection: {
            anchor: selection.from + prefix.length,
            head: selection.from + prefix.length + selectedText.length,
          },
        });
        return true;
      },
    [],
  );

  const applyLinkFormatting = useCallback((view: EditorView) => {
    const selection = view.state.selection.main;
    const selectedText = view.state.doc.sliceString(
      selection.from,
      selection.to,
    );
    const linkText = selectedText || "link text";
    const formattedText = `[${linkText}](https://)`;

    view.dispatch({
      changes: {
        from: selection.from,
        to: selection.to,
        insert: formattedText,
      },
      selection: selectedText
        ? {
            anchor: selection.from + 1,
            head: selection.from + 1 + linkText.length,
          }
        : {
            anchor: selection.from + 1,
            head: selection.from + 1 + "link text".length,
          },
    });
    return true;
  }, []);

  const applyQuoteFormatting = useCallback((view: EditorView) => {
    const selection = view.state.selection.main;
    const selectedText = view.state.doc.sliceString(
      selection.from,
      selection.to,
    );
    const text = selectedText || "quote";
    const quotedText = text
      .split("\n")
      .map((line) => `> ${line}`)
      .join("\n");

    view.dispatch({
      changes: { from: selection.from, to: selection.to, insert: quotedText },
      selection: {
        anchor: selection.from,
        head: selection.from + quotedText.length,
      },
    });
    return true;
  }, []);

  const [editing, setEditing] = useState(cell.source === "");
  const editorRef = useRef<CodeMirrorEditorRef>(null);
  const presence = usePresenceContext();
  const { extension: crdtBridgeExt } = useCrdtBridge(cell.id);
  const frameRef = useRef<IsolatedFrameHandle>(null);
  const viewRef = useRef<HTMLDivElement>(null);

  // Register EditorView with the cursor registry when in edit mode.
  const registeredViewRef = useRef<EditorView | null>(null);
  useEffect(() => {
    if (!editing) {
      if (registeredViewRef.current) {
        unregisterEditor(cell.id);
        unregisterAttributionEditor(cell.id);
        registeredViewRef.current = null;
      }
      return;
    }

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
  }, [cell.id, editing]);

  const darkMode = useDarkMode();

  const blobPort = useBlobPort();

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
    const processedSource = rewriteMarkdownAssetRefs(
      cell.source,
      cell.resolvedAssets,
      blobPort,
    );
    frameRef.current.render({
      mimeType: "text/markdown",
      data: processedSource,
      cellId: cell.id,
      replace: true,
    });
  }, [cell.source, cell.id, cell.resolvedAssets, blobPort]);

  // Sync markdown to iframe whenever source or resolved assets change (supports RTC updates)
  useEffect(() => {
    if (frameRef.current?.isReady && cell.source) {
      const processedSource = rewriteMarkdownAssetRefs(
        cell.source,
        cell.resolvedAssets,
        blobPort,
      );
      frameRef.current.render({
        mimeType: "text/markdown",
        data: processedSource,
        cellId: cell.id,
        replace: true,
      });
    }
  }, [cell.source, cell.id, cell.resolvedAssets, blobPort]);

  // Handle link clicks from iframe - open in system browser
  const handleLinkClick = useCallback((url: string) => {
    openUrl(url);
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

  // Search highlight extension for edit mode + remote cursors + presence sender
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
    onExecute: () => {}, // No-op for markdown, enables Shift+Enter navigation
    onDelete,
    cellId: cell.id,
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
      {
        key: "Mod-b",
        run: applyInlineFormatting("**"),
      },
      {
        key: "Mod-i",
        run: applyInlineFormatting("*"),
      },
      {
        key: "Mod-u",
        run: applyInlineFormatting("<u>", "</u>"),
      },
      {
        key: "Mod-k",
        run: applyLinkFormatting,
      },
      {
        key: "Mod-Shift-.",
        run: applyQuoteFormatting,
      },
      {
        key: "Mod-Shift->",
        run: applyQuoteFormatting,
      },
    ],
    [
      navigationKeyMap,
      cell.source,
      applyInlineFormatting,
      applyLinkFormatting,
      applyQuoteFormatting,
    ],
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
      isPreviousCellFromFocused={isPreviousCellFromFocused}
      isNextCellFromFocused={isNextCellFromFocused}
      onFocus={onFocus}
      presenceIndicators={<CellPresenceIndicators cellId={cell.id} />}
      dragHandleProps={dragHandleProps}
      isDragging={isDragging}
      rightGutterContent={
        editing ? (
          rightGutterContent
        ) : (
          <div className="flex flex-col gap-0.5">
            <button
              type="button"
              tabIndex={-1}
              onClick={() => setEditing(true)}
              className="flex items-center justify-center rounded p-1 text-muted-foreground/40 transition-colors hover:text-foreground"
              title="Edit"
            >
              <Pencil className="h-3.5 w-3.5" />
            </button>
            {rightGutterContent}
          </div>
        )
      }
      codeContent={
        <>
          {/* Editor section - hidden when not editing */}
          <div className={editing ? "block" : "hidden"}>
            <div className="flex items-center gap-1 py-1">
              <span className="text-xs text-muted-foreground font-mono">
                md
              </span>
            </div>
            <div>
              <CodeMirrorEditor
                ref={editorRef}
                initialValue={cell.source}
                language="markdown"
                lineWrapping
                onBlur={handleBlur}
                keyMap={keyMap}
                extensions={[crdtBridgeExt, ...searchExtensions]}
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
            className={cn("py-2 cursor-text outline-none", editing && "hidden")}
            onDoubleClick={handleDoubleClick}
            onKeyDown={handleViewKeyDown}
          >
            {/* Always render IsolatedFrame to preload it (hidden when no content) */}
            <div className={cell.source ? undefined : "hidden"}>
              <IsolatedFrame
                ref={frameRef}
                darkMode={darkMode}
                minHeight={24}
                autoHeight
                revealOnRender
                onReady={handleFrameReady}
                onLinkClick={handleLinkClick}
                onDoubleClick={handleDoubleClick}
                onError={handleIframeError}
                className="w-full"
              />
            </div>
            {!cell.source && (
              <p className="text-muted-foreground italic">
                Double-click to edit
              </p>
            )}
          </div>
        </>
      }
    />
  );
});
