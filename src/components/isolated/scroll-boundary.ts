import type { NteractWheelBoundaryParams } from "./rpc-methods";

function hasScrollableOverflow(element: HTMLElement): boolean {
  const { overflowY } = window.getComputedStyle(element);
  return overflowY === "auto" || overflowY === "scroll" || overflowY === "overlay";
}

function canScrollVertically(element: HTMLElement): boolean {
  return hasScrollableOverflow(element) && element.scrollHeight > element.clientHeight + 1;
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
    scrollTarget.scrollBy({ top: deltaY, behavior: "auto" });
    return;
  }

  iframe?.ownerDocument.defaultView?.scrollBy({ top: deltaY, behavior: "auto" });
}
