/**
 * Highlight all occurrences of a search query within a DOM container.
 *
 * Walks text nodes using TreeWalker, wraps matches in <mark> elements.
 * Returns a cleanup function that removes all marks and restores text nodes.
 */
export function highlightTextInDom(
  container: HTMLElement,
  query: string,
): () => void {
  if (!query) return () => {};

  const marks: HTMLElement[] = [];
  const walker = document.createTreeWalker(
    container,
    NodeFilter.SHOW_TEXT,
    null,
  );
  const lowerQuery = query.toLowerCase();

  // Collect matches first (to avoid mutating DOM while walking)
  const matches: { node: Text; offset: number; length: number }[] = [];
  let node = walker.nextNode();
  while (node) {
    const text = node.nodeValue || "";
    const lowerText = text.toLowerCase();
    let pos = lowerText.indexOf(lowerQuery, 0);
    while (pos !== -1) {
      matches.push({ node: node as Text, offset: pos, length: query.length });
      pos = lowerText.indexOf(lowerQuery, pos + query.length);
    }
    node = walker.nextNode();
  }

  // Apply highlights in reverse order to preserve offsets
  for (let i = matches.length - 1; i >= 0; i--) {
    const m = matches[i];
    try {
      const range = document.createRange();
      range.setStart(m.node, m.offset);
      range.setEnd(m.node, m.offset + m.length);
      const mark = document.createElement("mark");
      mark.className = "global-find-match";
      mark.style.cssText =
        "background: #fbbf24; color: #000; border-radius: 2px; padding: 0;";
      range.surroundContents(mark);
      marks.unshift(mark);
    } catch {
      // surroundContents can fail if range crosses element boundaries
    }
  }

  // Return cleanup function
  return () => {
    for (const mark of marks) {
      const parent = mark.parentNode;
      if (parent) {
        while (mark.firstChild) {
          parent.insertBefore(mark.firstChild, mark);
        }
        parent.removeChild(mark);
        parent.normalize();
      }
    }
  };
}
