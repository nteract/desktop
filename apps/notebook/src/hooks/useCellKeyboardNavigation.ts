import type { EditorView, KeyBinding } from "@codemirror/view";
import { useMemo, useRef } from "react";
import { logger } from "../lib/logger";

interface UseCellKeyboardNavigationOptions {
  onFocusPrevious: (cursorPosition: "start" | "end") => void;
  onFocusNext: (cursorPosition: "start" | "end") => void;
  onExecute?: () => void;
  onExecuteAndInsert?: () => void;
  onDelete?: () => void;
  /** Cell ID for debug logging */
  cellId?: string;
}

/**
 * Creates stable CodeMirror keybindings for cell navigation.
 *
 * Uses refs internally so the keybindings always call the latest callbacks,
 * even if CodeMirror doesn't re-apply the keymap extension after a re-render.
 * This is critical for drag-and-drop reordering where cell indices change
 * but the EditorView instance persists.
 */
export function useCellKeyboardNavigation({
  onFocusPrevious,
  onFocusNext,
  onExecute,
  onExecuteAndInsert,
  onDelete,
  cellId,
}: UseCellKeyboardNavigationOptions): KeyBinding[] {
  // Store callbacks in refs so keybindings always access current versions
  const onFocusPreviousRef = useRef(onFocusPrevious);
  const onFocusNextRef = useRef(onFocusNext);
  const onExecuteRef = useRef(onExecute);
  const onExecuteAndInsertRef = useRef(onExecuteAndInsert);
  const onDeleteRef = useRef(onDelete);
  const cellIdRef = useRef(cellId);

  // Update refs on every render
  onFocusPreviousRef.current = onFocusPrevious;
  onFocusNextRef.current = onFocusNext;
  onExecuteRef.current = onExecute;
  onExecuteAndInsertRef.current = onExecuteAndInsert;
  onDeleteRef.current = onDelete;
  cellIdRef.current = cellId;

  // Memoize keybindings - they're stable because they read from refs.
  // The keybindings close over refs, not the callbacks directly, so we only
  // need to recreate when the presence of optional callbacks changes.
  return useMemo(
    () => [
      {
        key: "ArrowUp",
        run: (view) => {
          const { from } = view.state.selection.main;
          if (from === 0) {
            logger.debug(`[cell-nav] ArrowUp at top of cell ${cellIdRef.current?.slice(0, 8)}`);
            onFocusPreviousRef.current("end");
            return true;
          }
          return false;
        },
      },
      {
        key: "ArrowDown",
        run: (view) => {
          const { from } = view.state.selection.main;
          const docLength = view.state.doc.length;
          if (from === docLength) {
            logger.debug(
              `[cell-nav] ArrowDown at bottom of cell ${cellIdRef.current?.slice(0, 8)}`,
            );
            onFocusNextRef.current("start");
            return true;
          }
          return false;
        },
      },
      ...(onDelete
        ? [
            {
              key: "Backspace",
              run: (view: EditorView) => {
                const { from } = view.state.selection.main;
                const docLength = view.state.doc.length;
                // Delete cell if cursor at start AND cell is empty
                if (from === 0 && docLength === 0) {
                  onFocusPreviousRef.current("end");
                  onDeleteRef.current?.();
                  return true;
                }
                return false;
              },
            },
          ]
        : []),
      ...(onExecute
        ? [
            {
              key: "Shift-Enter",
              run: () => {
                onExecuteRef.current?.();
                onFocusNextRef.current("start");
                return true;
              },
            },
            {
              key: "Mod-Enter",
              run: () => {
                onExecuteRef.current?.();
                return true;
              },
            },
            {
              key: "Ctrl-Enter",
              run: () => {
                onExecuteRef.current?.();
                return true;
              },
            },
          ]
        : []),
      ...(onExecuteAndInsert
        ? [
            {
              key: "Alt-Enter",
              run: () => {
                onExecuteAndInsertRef.current?.();
                return true;
              },
            },
          ]
        : []),
    ],
    // Only recreate if the presence of optional callbacks changes
    [!!onExecute, !!onExecuteAndInsert, !!onDelete],
  );
}
