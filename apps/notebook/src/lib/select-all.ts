import { EditorView } from "@codemirror/view";

function selectAllInCodeMirror(activeElement: Element): boolean {
  const cmContent =
    activeElement.closest(".cm-content") ??
    activeElement.closest(".cm-editor")?.querySelector(".cm-content");

  if (!(cmContent instanceof HTMLElement)) return false;

  const view = EditorView.findFromDOM(cmContent);
  if (!view) return false;

  const docLength = view.state.doc.length;
  view.focus();
  view.dispatch({
    selection: { anchor: 0, head: docLength },
    scrollIntoView: true,
  });
  return true;
}

function selectAllInContentEditable(activeElement: HTMLElement): boolean {
  const selection = activeElement.ownerDocument.defaultView?.getSelection();
  if (!selection) return false;

  const range = activeElement.ownerDocument.createRange();
  range.selectNodeContents(activeElement);
  selection.removeAllRanges();
  selection.addRange(range);
  return true;
}

export function selectAllInActiveElement(doc: Document = document): boolean {
  const activeElement = doc.activeElement;
  if (!activeElement) {
    return doc.execCommand?.("selectAll") ?? false;
  }

  if (selectAllInCodeMirror(activeElement)) {
    return true;
  }

  if (
    activeElement instanceof HTMLInputElement ||
    activeElement instanceof HTMLTextAreaElement
  ) {
    activeElement.select();
    return true;
  }

  if (activeElement instanceof HTMLElement && activeElement.isContentEditable) {
    return selectAllInContentEditable(activeElement);
  }

  return doc.execCommand?.("selectAll") ?? false;
}
