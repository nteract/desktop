// @vitest-environment jsdom
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { presenceSenderExtension } from "../presence-sender";

describe("presenceSenderExtension", () => {
  let view: EditorView;
  let onCursor: ReturnType<typeof vi.fn>;
  let onSelection: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    vi.useFakeTimers();
    onCursor = vi.fn();
    onSelection = vi.fn();
  });

  afterEach(() => {
    view?.destroy();
    vi.useRealTimers();
  });

  function createView(doc: string, focused = true) {
    const state = EditorState.create({
      doc,
      extensions: [
        presenceSenderExtension("cell-1", {
          onCursor,
          onSelection,
        }),
      ],
    });
    const v = new EditorView({ state });
    // In jsdom, hasFocus is always false — override for test control.
    if (focused) {
      Object.defineProperty(v, "hasFocus", {
        value: true,
        writable: true,
        configurable: true,
      });
    }
    return v;
  }

  it("sends cursor position when selection changes to a point", () => {
    view = createView("hello\nworld");

    // Move cursor to line 1, column 3 (0-based: line 0, col 3)
    view.dispatch({
      selection: { anchor: 3 },
    });

    // Should send immediately
    expect(onCursor).toHaveBeenCalledTimes(1);
    expect(onCursor).toHaveBeenCalledWith("cell-1", 0, 3);
    expect(onSelection).not.toHaveBeenCalled();
  });

  it("sends selection when anchor differs from head", () => {
    view = createView("hello\nworld");

    // Select "ell" (positions 1-4)
    view.dispatch({
      selection: { anchor: 1, head: 4 },
    });

    expect(onSelection).toHaveBeenCalledTimes(1);
    expect(onSelection).toHaveBeenCalledWith("cell-1", 0, 1, 0, 4);
    expect(onCursor).not.toHaveBeenCalled();
  });

  it("converts multi-line positions correctly", () => {
    view = createView("hello\nworld\nfoo");

    // Move cursor to "w" in "world" (line 1, col 0)
    view.dispatch({
      selection: { anchor: 6 },
    });

    expect(onCursor).toHaveBeenCalledWith("cell-1", 1, 0);
  });

  it("throttles rapid selection changes", () => {
    view = createView("hello");

    // First change - sent immediately
    view.dispatch({ selection: { anchor: 1 } });
    expect(onCursor).toHaveBeenCalledTimes(1);

    // Second change within throttle window - not sent yet
    view.dispatch({ selection: { anchor: 2 } });
    expect(onCursor).toHaveBeenCalledTimes(1);

    // Third change within throttle window - still not sent
    view.dispatch({ selection: { anchor: 3 } });
    expect(onCursor).toHaveBeenCalledTimes(1);

    // Advance past throttle interval (75ms)
    vi.advanceTimersByTime(75);

    // Now the pending update is sent (the last position: 3)
    expect(onCursor).toHaveBeenCalledTimes(2);
    expect(onCursor).toHaveBeenLastCalledWith("cell-1", 0, 3);
  });

  it("does not call callbacks for non-selection transactions", () => {
    view = createView("hello");

    // Initial selection
    view.dispatch({ selection: { anchor: 1 } });
    expect(onCursor).toHaveBeenCalledTimes(1);

    vi.advanceTimersByTime(100);

    // Document change without selection change
    view.dispatch({
      changes: { from: 5, insert: "!" },
    });

    // No additional cursor call
    expect(onCursor).toHaveBeenCalledTimes(1);
  });

  it("handles selection spanning multiple lines", () => {
    view = createView("hello\nworld\nfoo");

    // Select from "llo" to "wor" (crossing line boundary)
    view.dispatch({
      selection: { anchor: 2, head: 9 },
    });

    expect(onSelection).toHaveBeenCalledWith("cell-1", 0, 2, 1, 3);
  });

  it("does not send stale cursor from throttle after editor loses focus", () => {
    view = createView("hello");

    // First change — sent immediately, starts throttle window
    view.dispatch({ selection: { anchor: 1 } });
    expect(onCursor).toHaveBeenCalledTimes(1);

    // Second change within throttle — marks pendingUpdate
    view.dispatch({ selection: { anchor: 3 } });
    expect(onCursor).toHaveBeenCalledTimes(1);

    // Simulate losing focus (user clicked into another cell)
    (view as unknown as Record<string, unknown>).hasFocus = false;

    // Advance past throttle interval
    vi.advanceTimersByTime(75);

    // The pending update should NOT have been sent — the editor lost focus
    expect(onCursor).toHaveBeenCalledTimes(1);
  });

  it("does not send cursor when selection changes in unfocused editor", () => {
    view = createView("hello", false);

    view.dispatch({ selection: { anchor: 2 } });

    // Should not send — editor is unfocused
    expect(onCursor).not.toHaveBeenCalled();
  });
});
