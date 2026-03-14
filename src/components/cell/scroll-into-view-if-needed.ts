function isScrollable(overflowValue: string) {
  return /(auto|scroll|overlay)/.test(overflowValue);
}

function findScrollContainer(element: HTMLElement): HTMLElement | null {
  let current = element.parentElement;

  while (current) {
    const style = window.getComputedStyle(current);
    if (isScrollable(style.overflowY) || isScrollable(style.overflow)) {
      return current;
    }
    current = current.parentElement;
  }

  return null;
}

export function scrollElementIntoViewIfNeeded(element: HTMLElement) {
  const scrollContainer = findScrollContainer(element);
  if (!scrollContainer) {
    element.scrollIntoView({
      behavior: "smooth",
      block: "nearest",
      inline: "nearest",
    });
    return;
  }

  const elementRect = element.getBoundingClientRect();
  const containerRect = scrollContainer.getBoundingClientRect();

  if (
    elementRect.top >= containerRect.top &&
    elementRect.bottom <= containerRect.bottom
  ) {
    return;
  }

  const topOffset = elementRect.top - containerRect.top;
  const bottomOffset = elementRect.bottom - containerRect.bottom;
  const scrollTop =
    topOffset < 0
      ? scrollContainer.scrollTop + topOffset
      : scrollContainer.scrollTop + bottomOffset;

  scrollContainer.scrollTo({
    top: scrollTop,
    behavior: "smooth",
  });
}
