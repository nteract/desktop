// @vitest-environment jsdom
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import type { NotebookHost } from "@nteract/notebook-host";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import { startWindowFocusHandler } from "../window-focus";

function makeHost(): NotebookHost {
  return {
    window: {
      onFocusChange: vi.fn(() => vi.fn()),
    },
  } as unknown as NotebookHost;
}

function createEditor(doc = "hello"): EditorView {
  const parent = document.createElement("div");
  document.body.appendChild(parent);

  const view = new EditorView({
    parent,
    state: EditorState.create({ doc }),
  });

  Object.defineProperty(view, "hasFocus", {
    value: true,
    writable: true,
    configurable: true,
  });

  return view;
}

function focusEditor(view: EditorView): void {
  view.contentDOM.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
}

describe("window-focus", () => {
  let cleanup: (() => void) | undefined;
  let view: EditorView | undefined;

  afterEach(() => {
    cleanup?.();
    cleanup = undefined;
    view?.destroy();
    view = undefined;
    document.body.replaceChildren();
    vi.restoreAllMocks();
  });

  it("restores the focused editor after a real window blur/focus cycle", () => {
    cleanup = startWindowFocusHandler(makeHost());
    view = createEditor();
    focusEditor(view);

    const focusSpy = vi.spyOn(view, "focus").mockImplementation(() => undefined);
    const blurSpy = vi.spyOn(view.contentDOM, "blur").mockImplementation(() => undefined);
    const measureSpy = vi.spyOn(view, "requestMeasure").mockImplementation(() => undefined);

    window.dispatchEvent(new Event("blur"));
    window.dispatchEvent(new Event("focus"));

    expect(blurSpy).toHaveBeenCalledTimes(1);
    expect(focusSpy).toHaveBeenCalledTimes(1);
    expect(measureSpy).toHaveBeenCalled();
  });

  it("does not restore the previous editor when focus returns from an iframe output", () => {
    cleanup = startWindowFocusHandler(makeHost());
    view = createEditor();
    focusEditor(view);

    const focusSpy = vi.spyOn(view, "focus").mockImplementation(() => undefined);
    const blurSpy = vi.spyOn(view.contentDOM, "blur").mockImplementation(() => undefined);
    const measureSpy = vi.spyOn(view, "requestMeasure").mockImplementation(() => undefined);

    const iframe = document.createElement("iframe");
    document.body.appendChild(iframe);
    iframe.focus();

    window.dispatchEvent(new Event("blur"));
    window.dispatchEvent(new Event("focus"));

    expect(blurSpy).not.toHaveBeenCalled();
    expect(focusSpy).not.toHaveBeenCalled();
    expect(measureSpy).not.toHaveBeenCalled();
  });

  it("uses iframe pointerdown as an early guard before window blur snapshots selection", () => {
    cleanup = startWindowFocusHandler(makeHost());
    view = createEditor();
    focusEditor(view);

    const focusSpy = vi.spyOn(view, "focus").mockImplementation(() => undefined);
    const blurSpy = vi.spyOn(view.contentDOM, "blur").mockImplementation(() => undefined);

    const iframe = document.createElement("iframe");
    document.body.appendChild(iframe);
    iframe.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true }));

    window.dispatchEvent(new Event("blur"));
    window.dispatchEvent(new Event("focus"));

    expect(blurSpy).not.toHaveBeenCalled();
    expect(focusSpy).not.toHaveBeenCalled();
  });
});
