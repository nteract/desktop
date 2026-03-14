import { EditorView } from "@codemirror/view";
import { afterEach, describe, expect, it } from "vitest";
import { selectAllInActiveElement } from "../select-all";

describe("selectAllInActiveElement", () => {
  afterEach(() => {
    document.body.innerHTML = "";
  });

  it("selects the active CodeMirror document instead of the whole page", () => {
    const parent = document.createElement("div");
    document.body.appendChild(parent);

    const view = new EditorView({
      doc: "print('hello')",
      parent,
    });

    view.focus();

    expect(selectAllInActiveElement()).toBe(true);
    expect(view.state.selection.main.from).toBe(0);
    expect(view.state.selection.main.to).toBe(view.state.doc.length);

    view.destroy();
  });

  it("falls back to selecting a standard input", () => {
    const input = document.createElement("input");
    input.value = "desktop";
    document.body.appendChild(input);

    input.focus();

    expect(selectAllInActiveElement()).toBe(true);
    expect(input.selectionStart).toBe(0);
    expect(input.selectionEnd).toBe(input.value.length);
  });
});
