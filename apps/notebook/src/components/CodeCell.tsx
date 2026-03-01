import type { KeyBinding } from "@codemirror/view";
import { Trash2, X } from "lucide-react";
import {
  lazy,
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
import { searchHighlight } from "@/components/editor/search-highlight";
import { AnsiOutput } from "@/components/outputs/ansi-output";
import { ErrorBoundary } from "@/lib/error-boundary";
import type { CellPagePayload, MimeBundle } from "../App";
import { useCellKeyboardNavigation } from "../hooks/useCellKeyboardNavigation";
import { useEditorRegistry } from "../hooks/useEditorRegistry";
import { kernelCompletionExtension } from "../lib/kernel-completion";
import type { CodeCell as CodeCellType } from "../types";

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
  pagePayload: CellPagePayload | null;
  searchQuery?: string;
  searchActiveOffset?: number;
  onFocus: () => void;
  onUpdateSource: (source: string) => void;
  onExecute: () => void;
  onInterrupt: () => void;
  onDelete: () => void;
  onFocusPrevious?: (cursorPosition: "start" | "end") => void;
  onFocusNext?: (cursorPosition: "start" | "end") => void;
  onInsertCellAfter?: () => void;
  onClearPagePayload?: () => void;
  onFormat?: () => void;
  isLastCell?: boolean;
}

export function CodeCell({
  cell,
  language = "python",
  isFocused,
  isExecuting,
  pagePayload,
  searchQuery,
  searchActiveOffset = -1,
  onFocus,
  onUpdateSource,
  onExecute,
  onInterrupt,
  onDelete,
  onFocusPrevious,
  onFocusNext,
  onInsertCellAfter,
  onClearPagePayload,
  onFormat,
  isLastCell = false,
}: CodeCellProps) {
  const editorRef = useRef<CodeMirrorEditorRef>(null);
  const { registerEditor, unregisterEditor } = useEditorRegistry();
  const [historyDialogOpen, setHistoryDialogOpen] = useState(false);

  // Register editor with the registry for cross-cell navigation
  useEffect(() => {
    if (editorRef.current) {
      registerEditor(cell.id, {
        focus: () => editorRef.current?.focus(),
        setCursorPosition: (position) =>
          editorRef.current?.setCursorPosition(position),
      });
    }
    return () => unregisterEditor(cell.id);
  }, [cell.id, registerEditor, unregisterEditor]);

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
    onFormat,
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

  // Handle history selection - replace cell content
  const handleHistorySelect = useCallback(
    (source: string) => {
      onUpdateSource(source);
    },
    [onUpdateSource],
  );

  // Merge navigation keybindings (navigation bindings take precedence for Shift-Enter)
  const keyMap: KeyBinding[] = useMemo(
    () => [...navigationKeyMap, historyKeyBinding],
    [navigationKeyMap, historyKeyBinding],
  );

  // CodeMirror extensions: kernel completion + search highlighting
  const editorExtensions = useMemo(
    () => [
      kernelCompletionExtension,
      ...searchHighlight(searchQuery || "", searchActiveOffset),
    ],
    [searchQuery, searchActiveOffset],
  );

  const handleExecute = useCallback(() => {
    handleExecuteWithClear();
  }, [handleExecuteWithClear]);

  const gutterContent = (
    <CompactExecutionButton
      count={cell.execution_count}
      isExecuting={isExecuting}
      onExecute={handleExecute}
      onInterrupt={onInterrupt}
    />
  );

  const rightGutterContent = (
    <button
      type="button"
      tabIndex={-1}
      onClick={onDelete}
      className="flex items-center justify-center rounded p-1 text-muted-foreground/40 transition-colors hover:text-destructive"
      title="Delete cell"
    >
      <Trash2 className="h-3.5 w-3.5" />
    </button>
  );

  return (
    <>
      <CellContainer
        id={cell.id}
        cellType="code"
        isFocused={isFocused}
        onFocus={onFocus}
        gutterContent={gutterContent}
        rightGutterContent={rightGutterContent}
        codeContent={
          <>
            {/* Editor */}
            <div>
              <CodeMirrorEditor
                ref={editorRef}
                value={cell.source}
                language={language}
                onValueChange={onUpdateSource}
                keyMap={keyMap}
                extensions={editorExtensions}
                placeholder="Enter code..."
                className="min-h-[2rem]"
                autoFocus={isFocused}
              />
            </div>

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
          <OutputArea
            outputs={cell.outputs}
            preloadIframe
            searchQuery={searchQuery}
          />
        }
        hideOutput={cell.outputs.length === 0}
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
}
