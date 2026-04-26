import type { NteractWheelBoundaryParams } from "./rpc-methods";

function hasScrollableOverflow(element: HTMLElement): boolean {
  const { overflowY } = window.getComputedStyle(element);
  return overflowY === "auto" || overflowY === "scroll" || overflowY === "overlay";
}

function canScrollVertically(element: HTMLElement): boolean {
  return hasScrollableOverflow(element) && element.scrollHeight > element.clientHeight + 1;
}

function canConsumeScrollDelta(element: HTMLElement, deltaY: number): boolean {
  if (deltaY < 0) {
    return element.scrollTop > 0;
  }

  return element.scrollTop + element.clientHeight < element.scrollHeight - 1;
}

function canWindowConsumeScrollDelta(win: Window, deltaY: number): boolean {
  const doc = win.document.documentElement;
  const scrollTop = win.scrollY || doc.scrollTop;
  const viewportHeight = win.innerHeight;
  const scrollHeight = Math.max(doc.scrollHeight, win.document.body?.scrollHeight ?? 0);

  if (deltaY < 0) {
    return scrollTop > 0;
  }

  return scrollTop + viewportHeight < scrollHeight - 1;
}

export function findVerticalScrollAncestor(start: Element | null): HTMLElement | null {
  let element = start instanceof HTMLElement ? start : (start?.parentElement ?? null);

  while (element) {
    if (canScrollVertically(element)) {
      return element;
    }
    element = element.parentElement;
  }

  return null;
}

export function scrollFrameWheelBoundary(
  iframe: HTMLIFrameElement | null,
  params: NteractWheelBoundaryParams,
): void {
  const deltaY =
    typeof params.deltaY === "number" && Number.isFinite(params.deltaY) ? params.deltaY : 0;

  if (deltaY === 0) {
    return;
  }

  const scrollTarget = findVerticalScrollAncestor(iframe?.parentElement ?? null);
  if (scrollTarget) {
    if (!canConsumeScrollDelta(scrollTarget, deltaY)) {
      return;
    }
    scrollTarget.scrollBy({ top: deltaY, behavior: "auto" });
    return;
  }

  const win = iframe?.ownerDocument.defaultView;
  if (win && canWindowConsumeScrollDelta(win, deltaY)) {
    win.scrollBy({ top: deltaY, behavior: "auto" });
  }
}
