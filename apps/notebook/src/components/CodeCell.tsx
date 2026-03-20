import type { EditorView, KeyBinding } from "@codemirror/view";
import { ChevronRight, Code2, EyeOff, Trash2, X } from "lucide-react";
import {
  lazy,
  memo,
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
import { AnsiOutput } from "@/components/outputs/ansi-output";
import { ErrorBoundary } from "@/lib/error-boundary";
import { cn } from "@/lib/utils";
import type { CellPagePayload, MimeBundle } from "../App";
import { usePresenceContext } from "../contexts/PresenceContext";
import { useCellKeyboardNavigation } from "../hooks/useCellKeyboardNavigation";
import { useCrdtBridge } from "../hooks/useCrdtBridge";
import {
  registerAttributionEditor,
  unregisterAttributionEditor,
} from "../lib/attribution-registry";
import { registerEditor, unregisterEditor } from "../lib/cursor-registry";
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

/** Page payload display component - Zed REPL style */
function PagePayloadDisplay({
  data,
  onDismiss,
}: {
  data: MimeBundle;
  onDismiss: () => void;
}) {
  const htmlContent = data["text/html"];
  const textContent = data["text/plain"];

  return (
    <div className="cm-page-payload">
      <div className="cm-page-payload-content">
        {typeof htmlContent === "string" ? (
          <div dangerouslySetInnerHTML={{ __html: htmlContent }} />
        ) : typeof textContent === "string" ? (
          <AnsiOutput className="cm-page-payload-text">
            {textContent}
          </AnsiOutput>
        ) : (
          <pre className="cm-page-payload-text">
            {JSON.stringify(data, null, 2)}
          </pre>
        )}
      </div>
      <div className="cm-page-payload-gutter">
        <button
          type="button"
          className="cm-page-payload-dismiss"
          onClick={onDismiss}
          title="Dismiss (Escape)"
        >
          <X className="h-3 w-3" />
        </button>
      </div>
    </div>
  );
}

interface CodeCellProps {
  cell: CodeCellType;
  language?: SupportedLanguage;
  isFocused: boolean;
  isExecuting: boolean;
  isQueued: boolean;
  pagePayload: CellPagePayload | null;
  searchQuery?: string;
  searchActiveOffset?: number;
  onSearchMatchCount?: (count: number) => void;
  onFocus: () => void;
  onExecute: () => void;
  onInterrupt: () => void;
  onDelete: () => void;
  onFocusPrevious?: (cursorPosition: "start" | "end") => void;
  onFocusNext?: (cursorPosition: "start" | "end") => void;
  onInsertCellAfter?: () => void;
  onClearPagePayload?: () => void;
  isLastCell?: boolean;
  /** Whether this cell is immediately before the focused cell */
  isPreviousCellFromFocused?: boolean;
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
  /** Whether any cell in a hidden group is currently executing */
  isGroupExecuting?: boolean;
  /** Number of error outputs across all cells in a hidden group */
  hiddenGroupErrorCount?: number;
}

export const CodeCell = memo(function CodeCell({
  cell,
  language = "python",
  isFocused,
  isExecuting,
  isQueued,
  pagePayload,
  searchQuery,
  searchActiveOffset = -1,
  onSearchMatchCount,
  onFocus,
  onExecute,
  onInterrupt,
  onDelete,
  onFocusPrevious,
  onFocusNext,
  onInsertCellAfter,
  onClearPagePayload,
  isLastCell = false,
  isPreviousCellFromFocused,
  dragHandleProps,
  isDragging,
  onToggleSourceHidden,
  onToggleOutputsHidden,
  hiddenGroupCount,
  onExpandHiddenGroup,
  isGroupExecuting,
  hiddenGroupErrorCount,
}: CodeCellProps) {
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

  // When both are hidden, show a single "Cell hidden" chip.
  // We check metadata only (not outputs.length) so the cell stays collapsed
  // when outputs are transiently cleared during re-execution.
  const bothHidden = isSourceHidden && isOutputsHidden;

  // Register EditorView with the cursor registry for remote cursor rendering.
  // We use a ref + polling approach because the EditorView is created async
  // by CodeMirrorEditor and isn't available on first render.
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

  // Handle Escape key to dismiss page payload
  useEffect(() => {
    if (!pagePayload || !isFocused) return;

    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClearPagePayload?.();
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [pagePayload, isFocused, onClearPagePayload]);

  // Clear page payload when cell is executed (before new results come in)
  const handleExecuteWithClear = useCallback(() => {
    onClearPagePayload?.();
    onExecute();
  }, [onExecute, onClearPagePayload]);

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
    onExecute: handleExecuteWithClear,
    onExecuteAndInsert: onInsertCellAfter
      ? () => {
          handleExecuteWithClear();
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

  const handleExecute = useCallback(() => {
    handleExecuteWithClear();
  }, [handleExecuteWithClear]);

  const handleLinkClick = useCallback((url: string) => openUrl(url), []);

  const gutterContent = bothHidden ? null : (
    <CompactExecutionButton
      count={cell.execution_count}
      isExecuting={isExecuting}
      isQueued={isQueued}
      onExecute={handleExecute}
      onInterrupt={onInterrupt}
    />
  );

  const rightGutterContent = (
    <div className="flex flex-col gap-0.5">
      {/* Toggle source visibility (not shown when both hidden - badges handle it) */}
      {onToggleSourceHidden && !bothHidden && (
        <button
          type="button"
          tabIndex={-1}
          onClick={() => onToggleSourceHidden(!isSourceHidden)}
          className={cn(
            "flex items-center justify-center rounded p-1 transition-colors hover:text-foreground",
            isSourceHidden
              ? "text-muted-foreground/70"
              : "text-muted-foreground/40",
          )}
          title={isSourceHidden ? "Show source" : "Hide source"}
        >
          <Code2 className="h-3.5 w-3.5" />
        </button>
      )}
      {/* Delete button */}
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
  );

  return (
    <>
      <CellContainer
        id={cell.id}
        cellType="code"
        isFocused={isFocused}
        isPreviousCellFromFocused={isPreviousCellFromFocused}
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
              <div className="flex justify-start">
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
                    "inline-flex items-center gap-1 px-2 py-0.5 text-xs text-muted-foreground hover:text-foreground bg-muted/50 hover:bg-muted rounded transition-colors",
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
              <div className="flex justify-start">
                <button
                  type="button"
                  onClick={() => onToggleSourceHidden?.(false)}
                  className="inline-flex items-center gap-1 px-2 py-0.5 text-xs text-muted-foreground hover:text-foreground bg-muted/50 hover:bg-muted rounded transition-colors"
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
                className="min-h-[2rem]"
                autoFocus={isFocused}
              />
            )}

            {/* Page Payload (documentation from ? or ??) */}
            {pagePayload && (
              <div className="px-2 py-1">
                <ErrorBoundary
                  resetKeys={[pagePayload.data]}
                  fallback={() => (
                    <div className="text-xs text-muted-foreground italic px-1 py-2">
                      Failed to render documentation
                    </div>
                  )}
                >
                  <PagePayloadDisplay
                    data={pagePayload.data}
                    onDismiss={() => onClearPagePayload?.()}
                  />
                </ErrorBoundary>
              </div>
            )}
          </>
        }
        outputContent={
          isOutputsHidden && cell.outputs.length > 0 ? (
            <div className="flex justify-start">
              <button
                type="button"
                onClick={() => onToggleOutputsHidden?.(false)}
                className="inline-flex items-center gap-1 px-2 py-0.5 text-xs text-muted-foreground hover:text-foreground bg-muted/50 hover:bg-muted rounded transition-colors"
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
              preloadIframe
              searchQuery={searchQuery}
              onSearchMatchCount={onSearchMatchCount}
              onLinkClick={handleLinkClick}
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
