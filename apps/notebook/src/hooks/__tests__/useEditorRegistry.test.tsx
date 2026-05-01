import { EditorView } from "@codemirror/view";
import { act, fireEvent, render, screen } from "@testing-library/react";
import React from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { EditorRegistryProvider, useEditorRegistry } from "../useEditorRegistry";

vi.mock("@codemirror/view", () => ({
  EditorView: {
    findFromDOM: vi.fn(),
  },
}));

vi.mock("../../lib/logger", () => ({
  logger: {
    debug: vi.fn(),
    warn: vi.fn(),
  },
}));

class MockResizeObserver {
  static instances: MockResizeObserver[] = [];
  callback: ResizeObserverCallback;
  observe = vi.fn();
  disconnect = vi.fn();

  constructor(callback: ResizeObserverCallback) {
    this.callback = callback;
    MockResizeObserver.instances.push(this);
  }

  trigger() {
    this.callback([], this);
  }
}

function FocusButton() {
  const { focusCell } = useEditorRegistry();
  return (
    <button type="button" onClick={() => focusCell("cell-1", "start")}>
      focus
    </button>
  );
}

function renderRegistry() {
  return render(
    <EditorRegistryProvider>
      <FocusButton />
    </EditorRegistryProvider>,
  );
}

function appendNotebookDom() {
  const scrollContainer = document.createElement("div");
  scrollContainer.style.overflowY = "auto";
  Object.defineProperty(scrollContainer, "clientHeight", { configurable: true, value: 200 });
  Object.defineProperty(scrollContainer, "scrollHeight", { configurable: true, value: 1000 });
  scrollContainer.getBoundingClientRect = vi.fn(
    () =>
      ({
        top: 0,
        bottom: 200,
      }) as DOMRect,
  );

  const content = document.createElement("div");
  const cell = document.createElement("div");
  cell.dataset.cellId = "cell-1";
  cell.scrollIntoView = vi.fn();

  const editor = document.createElement("div");
  editor.tabIndex = -1;
  editor.scrollIntoView = vi.fn();
  editor.getBoundingClientRect = vi.fn(
    () =>
      ({
        top: 260,
        bottom: 300,
      }) as DOMRect,
  );

  const cmContent = document.createElement("div");
  cmContent.className = "cm-content";
  editor.appendChild(cmContent);
  cell.appendChild(editor);
  content.appendChild(cell);
  scrollContainer.appendChild(content);
  document.body.appendChild(scrollContainer);

  return { cell, editor, cmContent, scrollContainer };
}

describe("EditorRegistryProvider", () => {
  const originalResizeObserver = globalThis.ResizeObserver;

  beforeEach(() => {
    vi.useFakeTimers();
    MockResizeObserver.instances = [];
    globalThis.ResizeObserver = MockResizeObserver as unknown as typeof ResizeObserver;
    vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
      callback(0);
      return 1;
    });
    vi.spyOn(window, "cancelAnimationFrame").mockImplementation(() => {});
  });

  afterEach(() => {
    vi.runOnlyPendingTimers();
    vi.useRealTimers();
    vi.restoreAllMocks();
    globalThis.ResizeObserver = originalResizeObserver;
    document.body.innerHTML = "";
  });

  it("keeps the focused editor visible while notebook content resizes", () => {
    const { cell, editor, cmContent } = appendNotebookDom();
    vi.mocked(EditorView.findFromDOM).mockReturnValue({
      dom: editor,
      focus: () => editor.focus(),
      state: { doc: { length: 12 } },
      dispatch: vi.fn(),
    } as unknown as EditorView);

    renderRegistry();
    fireEvent.click(screen.getByRole("button", { name: "focus" }));

    expect(cell.scrollIntoView).toHaveBeenCalledWith({ block: "nearest", behavior: "smooth" });
    expect(EditorView.findFromDOM).toHaveBeenCalledWith(cmContent);
    expect(MockResizeObserver.instances).toHaveLength(1);

    act(() => {
      MockResizeObserver.instances[0].trigger();
    });

    expect(editor.scrollIntoView).toHaveBeenCalledWith({ block: "nearest", behavior: "auto" });
  });

  it("stops pinning when the user starts scrolling manually", () => {
    const { editor, scrollContainer } = appendNotebookDom();
    vi.mocked(EditorView.findFromDOM).mockReturnValue({
      dom: editor,
      focus: () => editor.focus(),
      state: { doc: { length: 12 } },
      dispatch: vi.fn(),
    } as unknown as EditorView);

    renderRegistry();
    fireEvent.click(screen.getByRole("button", { name: "focus" }));
    vi.mocked(editor.scrollIntoView).mockClear();
    fireEvent.wheel(scrollContainer);

    act(() => {
      MockResizeObserver.instances[0].trigger();
    });

    expect(MockResizeObserver.instances[0].disconnect).toHaveBeenCalled();
    expect(editor.scrollIntoView).not.toHaveBeenCalledWith({ block: "nearest", behavior: "auto" });
  });

  it("stops pinning when the user scrolls with the keyboard", () => {
    const { editor, scrollContainer } = appendNotebookDom();
    vi.mocked(EditorView.findFromDOM).mockReturnValue({
      dom: editor,
      focus: () => editor.focus(),
      state: { doc: { length: 12 } },
      dispatch: vi.fn(),
    } as unknown as EditorView);

    renderRegistry();
    fireEvent.click(screen.getByRole("button", { name: "focus" }));
    vi.mocked(editor.scrollIntoView).mockClear();
    fireEvent.keyDown(scrollContainer, { key: "PageDown" });

    act(() => {
      MockResizeObserver.instances[0].trigger();
    });

    expect(MockResizeObserver.instances[0].disconnect).toHaveBeenCalled();
    expect(editor.scrollIntoView).not.toHaveBeenCalledWith({ block: "nearest", behavior: "auto" });
  });

  it("stops pinning after the time window elapses", () => {
    const { editor } = appendNotebookDom();
    vi.mocked(EditorView.findFromDOM).mockReturnValue({
      dom: editor,
      focus: () => editor.focus(),
      state: { doc: { length: 12 } },
      dispatch: vi.fn(),
    } as unknown as EditorView);

    renderRegistry();
    fireEvent.click(screen.getByRole("button", { name: "focus" }));
    vi.mocked(editor.scrollIntoView).mockClear();

    act(() => {
      vi.advanceTimersByTime(2500);
      MockResizeObserver.instances[0].trigger();
    });

    expect(MockResizeObserver.instances[0].disconnect).toHaveBeenCalled();
    expect(editor.scrollIntoView).not.toHaveBeenCalledWith({ block: "nearest", behavior: "auto" });
  });

  it("cleans up active pinning when the provider unmounts", () => {
    const { editor } = appendNotebookDom();
    vi.mocked(EditorView.findFromDOM).mockReturnValue({
      dom: editor,
      focus: () => editor.focus(),
      state: { doc: { length: 12 } },
      dispatch: vi.fn(),
    } as unknown as EditorView);

    const { unmount } = renderRegistry();
    fireEvent.click(screen.getByRole("button", { name: "focus" }));
    vi.mocked(editor.scrollIntoView).mockClear();
    unmount();

    act(() => {
      MockResizeObserver.instances[0].trigger();
    });

    expect(MockResizeObserver.instances[0].disconnect).toHaveBeenCalled();
    expect(editor.scrollIntoView).not.toHaveBeenCalledWith({ block: "nearest", behavior: "auto" });
  });
});
