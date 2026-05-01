import { EditorView } from "@codemirror/view";
import { createContext, type ReactNode, useCallback, useContext, useEffect, useRef } from "react";
import { logger } from "../lib/logger";

interface EditorRegistryContextType {
  focusCell: (cellId: string, cursorPosition: "start" | "end") => void;
  resnapCell: (cellId: string) => void;
  cancelResnapCell: () => void;
}

const EditorRegistryContext = createContext<EditorRegistryContextType | null>(null);

export const FOCUSED_CELL_RESNAP_DURATION_MS = 2500;

type FocusedCellResnapOptions = {
  durationMs?: number;
  onStop?: () => void;
};

function pulseLayoutForCell(cellElement: Element): void {
  const win = cellElement.ownerDocument.defaultView;
  if (!win) return;

  const scrollContainer = cellElement.closest('[data-notebook-scroll-container="true"]');
  scrollContainer?.dispatchEvent(new Event("scroll", { bubbles: true }));
  win.dispatchEvent(new Event("resize"));
}

function snapCellIntoView(cellElement: Element): void {
  if (!cellElement.isConnected) return;
  cellElement.scrollIntoView({ block: "nearest", behavior: "auto" });
  pulseLayoutForCell(cellElement);
}

// Outputs often render after Shift-Enter navigation. Keep the newly focused
// cell pinned briefly while the notebook DOM settles, then release manual scroll.
export function startFocusedCellResnap(
  cellElement: Element,
  options: FocusedCellResnapOptions = {},
): () => void {
  const win = cellElement.ownerDocument.defaultView;
  if (!win) return () => {};

  let active = true;
  let animationFrame: number | null = null;
  let timeoutId: number | null = null;

  const snapIntoView = () => {
    if (!active || animationFrame !== null) return;
    animationFrame = win.requestAnimationFrame(() => {
      animationFrame = null;
      if (!active || !cellElement.isConnected) return;
      snapCellIntoView(cellElement);
    });
  };

  const observedElement = cellElement.parentElement ?? cellElement;
  const resizeObserver =
    typeof win.ResizeObserver === "function" ? new win.ResizeObserver(snapIntoView) : null;
  resizeObserver?.observe(observedElement);

  const mutationObserver =
    typeof win.MutationObserver === "function" ? new win.MutationObserver(snapIntoView) : null;
  mutationObserver?.observe(observedElement, {
    childList: true,
    subtree: true,
  });

  const cleanup = () => {
    if (!active) return;
    active = false;
    if (animationFrame !== null) {
      win.cancelAnimationFrame(animationFrame);
      animationFrame = null;
    }
    if (timeoutId !== null) {
      win.clearTimeout(timeoutId);
      timeoutId = null;
    }
    resizeObserver?.disconnect();
    mutationObserver?.disconnect();
    options.onStop?.();
  };

  timeoutId = win.setTimeout(cleanup, options.durationMs ?? FOCUSED_CELL_RESNAP_DURATION_MS);

  return cleanup;
}

export function EditorRegistryProvider({ children }: { children: ReactNode }) {
  const stopFocusedCellResnapRef = useRef<(() => void) | null>(null);
  const resnappingCellElementRef = useRef<Element | null>(null);

  useEffect(() => {
    return () => stopFocusedCellResnapRef.current?.();
  }, []);

  const cancelResnapCell = useCallback(() => {
    stopFocusedCellResnapRef.current?.();
  }, []);

  const startOrReuseCellResnap = useCallback((cellElement: Element, snapNow: boolean) => {
    if (stopFocusedCellResnapRef.current && resnappingCellElementRef.current === cellElement) {
      if (snapNow) {
        snapCellIntoView(cellElement);
      } else {
        pulseLayoutForCell(cellElement);
      }
      return;
    }

    stopFocusedCellResnapRef.current?.();
    resnappingCellElementRef.current = cellElement;
    let cleanup: () => void = () => {};
    cleanup = startFocusedCellResnap(cellElement, {
      onStop: () => {
        if (resnappingCellElementRef.current === cellElement) {
          resnappingCellElementRef.current = null;
        }
        if (stopFocusedCellResnapRef.current === cleanup) {
          stopFocusedCellResnapRef.current = null;
        }
      },
    });
    stopFocusedCellResnapRef.current = cleanup;

    if (snapNow) {
      snapCellIntoView(cellElement);
    } else {
      pulseLayoutForCell(cellElement);
    }
  }, []);

  const resnapCell = useCallback(
    (cellId: string) => {
      const cellElement = document.querySelector(`[data-cell-id="${CSS.escape(cellId)}"]`);
      if (!cellElement) {
        logger.warn(`[cell-nav] Cell element not found: ${cellId.slice(0, 8)}`);
        return;
      }

      startOrReuseCellResnap(cellElement, true);
    },
    [startOrReuseCellResnap],
  );

  // Focus a cell's editor using DOM lookup - bypasses registration timing issues
  const focusCell = useCallback(
    (cellId: string, cursorPosition: "start" | "end") => {
      // Find the cell element by data attribute
      const cellElement = document.querySelector(`[data-cell-id="${CSS.escape(cellId)}"]`);
      if (!cellElement) {
        logger.warn(`[cell-nav] Cell element not found: ${cellId.slice(0, 8)}`);
        return;
      }

      startOrReuseCellResnap(cellElement, false);

      // Scroll the cell container into the notebook viewport
      cellElement.scrollIntoView({ block: "nearest", behavior: "smooth" });

      // Find CodeMirror's content element inside the cell
      const cmContent = cellElement.querySelector(".cm-content");
      if (!cmContent) {
        // Might be a markdown cell in view mode - no editor to focus
        logger.debug(`[cell-nav] No CM content in cell (markdown?): ${cellId.slice(0, 8)}`);
        return;
      }

      // Use CodeMirror's API to find the EditorView from DOM
      const view = EditorView.findFromDOM(cmContent as HTMLElement);
      if (!view) {
        logger.warn(`[cell-nav] EditorView not found for: ${cellId.slice(0, 8)}`);
        return;
      }

      // Set cursor position and focus
      const doc = view.state.doc;
      const pos = cursorPosition === "start" ? 0 : doc.length;
      view.dispatch({
        selection: { anchor: pos, head: pos },
        scrollIntoView: true,
      });
      view.focus();
    },
    [startOrReuseCellResnap],
  );

  return (
    <EditorRegistryContext.Provider value={{ cancelResnapCell, focusCell, resnapCell }}>
      {children}
    </EditorRegistryContext.Provider>
  );
}

export function useEditorRegistry() {
  const context = useContext(EditorRegistryContext);
  if (!context) {
    throw new Error("useEditorRegistry must be used within EditorRegistryProvider");
  }
  return context;
}
