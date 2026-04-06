import type { EditorView, KeyBinding } from "@codemirror/view";
import { ChevronRight, Code2, EyeOff } from "lucide-react";
import {
  lazy,
  memo,
  type ReactNode,
  Suspense,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { CellContainer } from "@/components/cell/CellContainer";
import { CompactExecutionButton } from "@/components/cell/CompactExecutionButton";
import { OutputArea } from "@/components/cell/OutputArea";
import {
  CodeMirrorEditor,
  type CodeMirrorEditorRef,
} from "@/components/editor/codemirror-editor";
import type { SupportedLanguage } from "@/components/editor/languages";
import { remoteCursorsExtension } from "@/components/editor/remote-cursors";
import { searchHighlight } from "@/components/editor/search-highlight";
import { textAttributionExtension } from "@/components/editor/text-attribution";
import { cn } from "@/lib/utils";
import { usePresenceContext } from "../contexts/PresenceContext";
import { useCellKeyboardNavigation } from "../hooks/useCellKeyboardNavigation";
import { useCrdtBridge } from "../hooks/useCrdtBridge";
import {
  useIsCellExecuting,
  useIsCellFocused,
  useIsCellQueued,
  useIsGroupExecuting,
  useIsNextCellFromFocused,
  useIsPreviousCellFromFocused,
  useSearchActiveOffset,
  useSearchQuery,
} from "../lib/cell-ui-state";
import {
  onEditorRegistered,
  onEditorUnregistered,
} from "../lib/cursor-registry";
import {
  registerCellEditor,
  unregisterCellEditor,
} from "../lib/editor-registry";
import { kernelCompletionExtension } from "../lib/kernel-completion";
import { openUrl } from "../lib/open-url";
import { presenceSenderExtension } from "../lib/presence-sender";
import { tabCompletionKeymap } from "../lib/tab-completion";
import type { CodeCell as CodeCellType } from "../types";
import { CellPresenceIndicators } from "./cell/CellPresenceIndicators";

// Lazy load HistorySearchDialog - it pulls in react-syntax-highlighter (~800KB)
// Only loaded when user opens history search (Ctrl+R)
const HistorySearchDialog = lazy(() =>
  import("./HistorySearchDialog").then((m) => ({
    default: m.HistorySearchDialog,
  })),
);

interface CodeCellProps {
  cell: CodeCellType;
  language?: SupportedLanguage;
  onSearchMatchCount?: (count: number) => void;
  onFocus: () => void;
  onExecute: () => void;
  onInterrupt: () => void;
  onDelete: () => void;
  onFocusPrevious?: (cursorPosition: "start" | "end") => void;
  onFocusNext?: (cursorPosition: "start" | "end") => void;
  onInsertCellAfter?: () => void;
  isLastCell?: boolean;
  /** Props for dnd-kit drag handle (applied to ribbon) */
  dragHandleProps?: Record<string, unknown>;
  /** Whether this cell is currently being dragged */
  isDragging?: boolean;
  /** Callback to toggle source visibility (JupyterLab convention) */
  onToggleSourceHidden?: (hidden: boolean) => void;
  /** Callback to toggle outputs visibility (JupyterLab convention) */
  onToggleOutputsHidden?: (hidden: boolean) => void;
  /** Number of consecutive fully-hidden cells in this group (including this one) */
  hiddenGroupCount?: number;
  /** Callback to expand all cells in a hidden group */
  onExpandHiddenGroup?: () => void;
  /** Cell IDs in this hidden group (for executing indicator) */
  hiddenGroupCellIds?: string[];
  /** Number of error outputs across all cells in a hidden group */
  hiddenGroupErrorCount?: number;
  /** Content for the right gutter (e.g., delete button, source toggle) */
  rightGutterContent?: ReactNode;
}

export const CodeCell = memo(function CodeCell({
  cell,
  language = "python",
  onSearchMatchCount,
  onFocus,
  onExecute,
  onInterrupt,
  onDelete,
  onFocusPrevious,
  onFocusNext,
  onInsertCellAfter,
  isLastCell = false,
  dragHandleProps,
  isDragging,
  onToggleSourceHidden,
  onToggleOutputsHidden,
  hiddenGroupCount,
  onExpandHiddenGroup,
  hiddenGroupCellIds,
  hiddenGroupErrorCount,
  rightGutterContent,
}: CodeCellProps) {
  // Read transient UI state from the store
  const isFocused = useIsCellFocused(cell.id);
  const isExecuting = useIsCellExecuting(cell.id);
  const isQueued = useIsCellQueued(cell.id);
  const isPreviousCellFromFocused = useIsPreviousCellFromFocused(cell.id);
  const isNextCellFromFocused = useIsNextCellFromFocused(cell.id);
  const searchQuery = useSearchQuery();
  const searchActiveOffset = useSearchActiveOffset(cell.id);
  const isGroupExecuting = useIsGroupExecuting(hiddenGroupCellIds);
  const editorRef = useRef<CodeMirrorEditorRef>(null);
  const [historyDialogOpen, setHistoryDialogOpen] = useState(false);
  const presence = usePresenceContext();
  const { extension: crdtBridgeExt, bridge } = useCrdtBridge(cell.id);

  // Check cell metadata for visibility (JupyterLab convention)
  const isSourceHidden =
    (cell.metadata?.jupyter as { source_hidden?: boolean })?.source_hidden ===
    true;
  const isOutputsHidden =
    (cell.metadata?.jupyter as { outputs_hidden?: boolean })?.outputs_hidden ===
    true;

  // Fully collapsed when source is hidden AND there's nothing else to show
  // (outputs explicitly hidden, or no outputs at all).
  const bothHidden =
    isSourceHidden && (isOutputsHidden || cell.outputs.length === 0);

  // Register EditorView with the cursor registry for remote cursor rendering.
  // We use a ref + polling approach because the EditorView is created async
  // by CodeMirrorEditor and isn't available on first render.
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

  // Get keyboard navigation bindings
  const navigationKeyMap = useCellKeyboardNavigation({
    onFocusPrevious: onFocusPrevious ?? (() => {}),
    onFocusNext: handleFocusNextOrCreate,
    onExecute: onExecute,
    onExecuteAndInsert: onInsertCellAfter
      ? () => {
          onExecute();
          onInsertCellAfter();
        }
      : undefined,
    onDelete,
    cellId: cell.id,
  });

  // Ctrl+R to open history search
  const historyKeyBinding: KeyBinding = useMemo(
    () => ({
      key: "Ctrl-r",
      run: () => {
        setHistoryDialogOpen(true);
        return true;
      },
    }),
    [],
  );

  // Handle history selection - replace cell content via CRDT bridge
  const handleHistorySelect = useCallback(
    (source: string) => {
      bridge.replaceSource(source);
    },
    [bridge],
  );

  // Merge navigation keybindings (navigation bindings take precedence for Shift-Enter)
  const keyMap: KeyBinding[] = useMemo(
    () => [...navigationKeyMap, historyKeyBinding],
    [navigationKeyMap, historyKeyBinding],
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

  // CodeMirror extensions: CRDT bridge + kernel completion + tab completion + search highlighting + remote cursors + presence sender
  const editorExtensions = useMemo(
    () => [
      crdtBridgeExt,
      kernelCompletionExtension,
      tabCompletionKeymap,
      ...searchHighlight(searchQuery || "", searchActiveOffset),
      ...remoteCursorsExt,
      ...textAttributionExt,
      ...presenceSenderExt,
    ],
    [
      crdtBridgeExt,
      searchQuery,
      searchActiveOffset,
      remoteCursorsExt,
      textAttributionExt,
      presenceSenderExt,
    ],
  );

  const handleLinkClick = useCallback((url: string) => openUrl(url), []);

  const gutterContent = bothHidden ? null : (
    <CompactExecutionButton
      count={cell.execution_count}
      isExecuting={isExecuting}
      isQueued={isQueued}
      onExecute={onExecute}
      onInterrupt={onInterrupt}
    />
  );

  return (
    <>
      <CellContainer
        id={cell.id}
        cellType="code"
        isFocused={isFocused}
        isPreviousCellFromFocused={isPreviousCellFromFocused}
        isNextCellFromFocused={isNextCellFromFocused}
        onFocus={onFocus}
        gutterContent={gutterContent}
        rightGutterContent={rightGutterContent}
        presenceIndicators={<CellPresenceIndicators cellId={cell.id} />}
        dragHandleProps={dragHandleProps}
        isDragging={isDragging}
        codeContent={
          <>
            {/* Source visibility toggle + Editor */}
            {bothHidden ? (
              <div className="flex items-center justify-start mt-0.5">
                <button
                  type="button"
                  onClick={() => {
                    if (onExpandHiddenGroup) {
                      onExpandHiddenGroup();
                    } else {
                      onToggleSourceHidden?.(false);
                      onToggleOutputsHidden?.(false);
                    }
                  }}
                  className={cn(
                    "inline-flex items-center gap-1 px-2 py-0.5 text-sm text-muted-foreground hover:text-foreground bg-muted/50 hover:bg-muted rounded transition-colors",
                    (isExecuting || isGroupExecuting) && "animate-pulse",
                  )}
                  title={
                    hiddenGroupCount && hiddenGroupCount > 1
                      ? `Show ${hiddenGroupCount} cells`
                      : "Show cell"
                  }
                >
                  <span>
                    {hiddenGroupCount && hiddenGroupCount > 1
                      ? `${hiddenGroupCount} cells hidden`
                      : "Cell hidden"}
                  </span>
                  {hiddenGroupErrorCount ? (
                    <span className="text-destructive font-medium">
                      {hiddenGroupErrorCount === 1
                        ? "1 error"
                        : `${hiddenGroupErrorCount} errors`}
                    </span>
                  ) : null}
                  <ChevronRight className="h-3 w-3" />
                </button>
              </div>
            ) : isSourceHidden ? (
              <div className="flex items-center justify-start mt-0.5">
                <button
                  type="button"
                  onClick={() => onToggleSourceHidden?.(false)}
                  className="inline-flex items-center gap-1 px-2 py-0.5 text-sm font-mono text-muted-foreground hover:text-foreground bg-muted/50 hover:bg-muted rounded transition-colors"
                  title="Show source"
                >
                  <Code2 className="h-3 w-3" />
                  <span className="font-mono truncate max-w-48">
                    {cell.source.split("\n")[0] || "source"}
                  </span>
                  <ChevronRight className="h-3 w-3" />
                </button>
              </div>
            ) : (
              <CodeMirrorEditor
                ref={editorRef}
                initialValue={cell.source}
                language={language}
                keyMap={keyMap}
                extensions={editorExtensions}
                placeholder="Enter code..."
                autoFocus={isFocused}
              />
            )}
          </>
        }
        outputContent={
          isOutputsHidden && cell.outputs.length > 0 ? (
            <div className="flex items-center justify-start mt-0.5 pl-6">
              <button
                type="button"
                onClick={() => onToggleOutputsHidden?.(false)}
                className="inline-flex items-center gap-1 px-2 py-0.5 text-sm text-muted-foreground hover:text-foreground bg-muted/50 hover:bg-muted rounded transition-colors"
                title="Show outputs"
              >
                <span>
                  {cell.outputs.length} output
                  {cell.outputs.length !== 1 ? "s" : ""}
                </span>
                <ChevronRight className="h-3 w-3" />
              </button>
            </div>
          ) : (
            <OutputArea
              outputs={cell.outputs}
              cellId={cell.id}
              preloadIframe
              searchQuery={searchQuery}
              onSearchMatchCount={onSearchMatchCount}
              onLinkClick={handleLinkClick}
              onIframeMouseDown={onFocus}
            />
          )
        }
        outputRightGutterContent={
          onToggleOutputsHidden &&
          cell.outputs.length > 0 &&
          !isOutputsHidden ? (
            <button
              type="button"
              tabIndex={-1}
              onClick={() => onToggleOutputsHidden(true)}
              className="flex items-center justify-center rounded p-1 text-muted-foreground/40 transition-colors hover:text-foreground"
              title="Hide outputs"
            >
              <EyeOff className="h-3.5 w-3.5" />
            </button>
          ) : undefined
        }
        hideOutput={cell.outputs.length === 0 || bothHidden}
      />

      {/* History Search Dialog (Ctrl+R) - lazy loaded */}
      {historyDialogOpen && (
        <Suspense fallback={null}>
          <HistorySearchDialog
            open={historyDialogOpen}
            onOpenChange={setHistoryDialogOpen}
            onSelect={handleHistorySelect}
          />
        </Suspense>
      )}
    </>
  );
});
