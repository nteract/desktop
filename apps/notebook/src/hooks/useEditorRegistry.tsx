import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useRef,
} from "react";
import { logger } from "../lib/logger";

export interface EditorRef {
  focus: () => void;
  setCursorPosition: (position: "start" | "end") => void;
}

interface EditorRegistryContextType {
  registerEditor: (cellId: string, ref: EditorRef) => void;
  unregisterEditor: (cellId: string) => void;
  focusCell: (cellId: string, cursorPosition: "start" | "end") => void;
}

const EditorRegistryContext = createContext<EditorRegistryContextType | null>(
  null,
);

export function EditorRegistryProvider({ children }: { children: ReactNode }) {
  const editorsRef = useRef<Map<string, EditorRef>>(new Map());

  const registerEditor = useCallback((cellId: string, ref: EditorRef) => {
    logger.debug(`[cell-nav] registerEditor: ${cellId.slice(0, 8)}`);
    editorsRef.current.set(cellId, ref);
  }, []);

  const unregisterEditor = useCallback((cellId: string) => {
    logger.debug(`[cell-nav] unregisterEditor: ${cellId.slice(0, 8)}`);
    editorsRef.current.delete(cellId);
  }, []);

  const focusCell = useCallback(
    (cellId: string, cursorPosition: "start" | "end") => {
      const tryFocus = () => {
        const editor = editorsRef.current.get(cellId);
        const registeredIds = Array.from(editorsRef.current.keys()).map((id) =>
          id.slice(0, 8),
        );
        logger.debug(
          `[cell-nav] focusCell: target=${cellId.slice(0, 8)} found=${!!editor} registered=[${registeredIds.join(",")}]`,
        );
        if (editor) {
          editor.setCursorPosition(cursorPosition);
          editor.focus();
          // Scroll the cell into view
          const cellElement = document.querySelector(
            `[data-cell-id="${cellId}"]`,
          );
          if (cellElement) {
            cellElement.scrollIntoView({
              behavior: "smooth",
              block: "nearest",
            });
          }
          return true;
        }
        return false;
      };

      if (!tryFocus()) {
        // Editor not found - might be mid-remount after drag, try once more after React settles
        queueMicrotask(() => {
          if (!tryFocus()) {
            logger.warn(
              `[cell-nav] Editor not found after retry: ${cellId.slice(0, 8)}`,
            );
          }
        });
      }
    },
    [],
  );

  return (
    <EditorRegistryContext.Provider
      value={{ registerEditor, unregisterEditor, focusCell }}
    >
      {children}
    </EditorRegistryContext.Provider>
  );
}

export function useEditorRegistry() {
  const context = useContext(EditorRegistryContext);
  if (!context) {
    throw new Error(
      "useEditorRegistry must be used within EditorRegistryProvider",
    );
  }
  return context;
}
