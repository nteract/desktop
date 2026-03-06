import type { EditorView, KeyBinding } from "@codemirror/view";

interface UseCellKeyboardNavigationOptions {
  onFocusPrevious: (cursorPosition: "start" | "end") => void;
  onFocusNext: (cursorPosition: "start" | "end") => void;
  onExecute?: () => void;
  onExecuteAndInsert?: () => void;
  onDelete?: () => void;
}

export function useCellKeyboardNavigation({
  onFocusPrevious,
  onFocusNext,
  onExecute,
  onExecuteAndInsert,
  onDelete,
}: UseCellKeyboardNavigationOptions): KeyBinding[] {
  return [
    {
      key: "ArrowUp",
      run: (view) => {
        const { from } = view.state.selection.main;
        if (from === 0) {
          onFocusPrevious("end");
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
          onFocusNext("start");
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
                onFocusPrevious("end");
                onDelete();
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
              onExecute();
              onFocusNext("start");
              return true;
            },
          },
          {
            key: "Mod-Enter",
            run: () => {
              onExecute();
              return true;
            },
          },
          {
            key: "Ctrl-Enter",
            run: () => {
              onExecute();
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
              onExecuteAndInsert();
              return true;
            },
          },
        ]
      : []),
  ];
}
