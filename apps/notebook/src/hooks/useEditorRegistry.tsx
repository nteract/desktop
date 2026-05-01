import { EditorView } from "@codemirror/view";
import { createContext, type ReactNode, useCallback, useContext, useEffect } from "react";
import { logger } from "../lib/logger";

interface EditorRegistryContextType {
  focusCell: (cellId: string, cursorPosition: "start" | "end") => void;
}

const EditorRegistryContext = createContext<EditorRegistryContextType | null>(null);
const SCROLL_PIN_DURATION_MS = 2500;
const SCROLL_PIN_MARGIN_PX = 12;

let cancelActiveScrollPin: (() => void) | null = null;
const SCROLL_KEYS = new Set(["PageUp", "PageDown", "Home", "End", " "]);

function getNearestScrollContainer(element: Element): HTMLElement | null {
  let current = element.parentElement;
  while (current) {
    const { overflowY } = window.getComputedStyle(current);
    if (overflowY === "auto" || overflowY === "scroll" || overflowY === "overlay") {
      return current;
    }
    current = current.parentElement;
  }
  return null;
}

function getScrollContentElement(container: HTMLElement, cellElement: Element): Element {
  let current = cellElement;
  while (current.parentElement && current.parentElement !== container) {
    current = current.parentElement;
  }
  return current.parentElement === container ? current : (container.firstElementChild ?? container);
}

function isAnchorVisible(container: HTMLElement, anchor: Element): boolean {
  const containerRect = container.getBoundingClientRect();
  const anchorRect = anchor.getBoundingClientRect();
  return (
    anchorRect.top >= containerRect.top + SCROLL_PIN_MARGIN_PX &&
    anchorRect.bottom <= containerRect.bottom - SCROLL_PIN_MARGIN_PX
  );
}

function shouldCancelScrollPinForKey(event: KeyboardEvent): boolean {
  if (!SCROLL_KEYS.has(event.key)) return false;
  const target = event.target;
  if (!(target instanceof HTMLElement)) return true;
  return (
    target === event.currentTarget ||
    !target.isContentEditable ||
    event.key === "PageUp" ||
    event.key === "PageDown"
  );
}

function startScrollPin(cellElement: Element, anchorElement: Element) {
  cancelActiveScrollPin?.();
  cancelActiveScrollPin = null;

  const scrollContainer = getNearestScrollContainer(cellElement);
  if (!scrollContainer || typeof ResizeObserver === "undefined") return;

  const contentElement = getScrollContentElement(scrollContainer, cellElement);
  let frameId: number | null = null;
  let timeoutId: number | null = null;
  let isCancelled = false;

  const keepAnchorVisible = () => {
    frameId = null;
    if (isCancelled) return;
    if (!cellElement.isConnected || !anchorElement.isConnected) {
      cleanup();
      return;
    }
    if (
      cellElement.contains(document.activeElement) &&
      !isAnchorVisible(scrollContainer, anchorElement)
    ) {
      anchorElement.scrollIntoView({ block: "nearest", behavior: "auto" });
    }
  };

  const scheduleKeepVisible = () => {
    if (frameId !== null || isCancelled) return;
    frameId = window.requestAnimationFrame(keepAnchorVisible);
  };

  const observer = new ResizeObserver(scheduleKeepVisible);
  const cleanupOnKeydown = (event: KeyboardEvent) => {
    if (shouldCancelScrollPinForKey(event)) {
      cleanup();
    }
  };

  function cleanup() {
    if (isCancelled) return;
    isCancelled = true;
    observer.disconnect();
    scrollContainer.removeEventListener("wheel", cleanup);
    scrollContainer.removeEventListener("touchstart", cleanup);
    scrollContainer.removeEventListener("keydown", cleanupOnKeydown);
    if (frameId !== null) {
      window.cancelAnimationFrame(frameId);
      frameId = null;
    }
    if (timeoutId !== null) {
      window.clearTimeout(timeoutId);
      timeoutId = null;
    }
    if (cancelActiveScrollPin === cleanup) {
      cancelActiveScrollPin = null;
    }
  }

  observer.observe(contentElement);
  scrollContainer.addEventListener("wheel", cleanup, { passive: true });
  scrollContainer.addEventListener("touchstart", cleanup, { passive: true });
  scrollContainer.addEventListener("keydown", cleanupOnKeydown);
  timeoutId = window.setTimeout(cleanup, SCROLL_PIN_DURATION_MS);
  scheduleKeepVisible();
  cancelActiveScrollPin = cleanup;
}

export function EditorRegistryProvider({ children }: { children: ReactNode }) {
  useEffect(() => {
    return () => {
      cancelActiveScrollPin?.();
    };
  }, []);

  // Focus a cell's editor using DOM lookup - bypasses registration timing issues
  const focusCell = useCallback((cellId: string, cursorPosition: "start" | "end") => {
    // Find the cell element by data attribute
    const cellElement = document.querySelector(`[data-cell-id="${CSS.escape(cellId)}"]`);
    if (!cellElement) {
      logger.warn(`[cell-nav] Cell element not found: ${cellId.slice(0, 8)}`);
      return;
    }

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
    startScrollPin(cellElement, view.dom);
  }, []);

  return (
    <EditorRegistryContext.Provider value={{ focusCell }}>
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
