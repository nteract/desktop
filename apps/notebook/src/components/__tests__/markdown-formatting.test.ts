/**
 * Tests for MarkdownCell's formatting logic:
 * - Inline formatting wraps selection correctly and repositions cursor
 * - Link formatting handles empty vs existing selection
 * - Quote formatting prefixes each line with "> "
 * - Edit/view toggle conditions (empty cell stays in edit, non-empty exits on blur)
 *
 * These test the pure formatting functions extracted from MarkdownCell,
 * not the React component itself.
 */

import { describe, expect, it } from "vite-plus/test";

// ── Extracted formatting logic from MarkdownCell.tsx ─────────────

interface MockSelection {
  from: number;
  to: number;
}

interface FormatResult {
  insert: string;
  from: number;
  to: number;
  /** New selection anchor (start) */
  selectionAnchor: number;
  /** New selection head (end) */
  selectionHead: number;
}

/**
 * Mirrors applyInlineFormatting from MarkdownCell.tsx.
 * Wraps the selected text with prefix/suffix and positions cursor
 * to select the original text (for chaining formats).
 */
function applyInlineFormatting(
  docText: string,
  selection: MockSelection,
  prefix: string,
  suffix = prefix,
): FormatResult {
  const selectedText = docText.slice(selection.from, selection.to);
  const wrappedText = `${prefix}${selectedText}${suffix}`;

  return {
    insert: wrappedText,
    from: selection.from,
    to: selection.to,
    selectionAnchor: selection.from + prefix.length,
    selectionHead: selection.from + prefix.length + selectedText.length,
  };
}

/**
 * Mirrors applyLinkFormatting from MarkdownCell.tsx.
 * With selected text: [selectedText](https://)
 * Without: [link text](https://)
 */
function applyLinkFormatting(docText: string, selection: MockSelection): FormatResult {
  const selectedText = docText.slice(selection.from, selection.to);
  const linkText = selectedText || "link text";
  const formattedText = `[${linkText}](https://)`;

  const anchor = selection.from + 1;
  const head = selectedText
    ? selection.from + 1 + linkText.length
    : selection.from + 1 + "link text".length;

  return {
    insert: formattedText,
    from: selection.from,
    to: selection.to,
    selectionAnchor: anchor,
    selectionHead: head,
  };
}

/**
 * Mirrors applyQuoteFormatting from MarkdownCell.tsx.
 * Prefixes each line with "> " and selects the result.
 */
function applyQuoteFormatting(docText: string, selection: MockSelection): FormatResult {
  const selectedText = docText.slice(selection.from, selection.to);
  const text = selectedText || "quote";
  const quotedText = text
    .split("\n")
    .map((line) => `> ${line}`)
    .join("\n");

  return {
    insert: quotedText,
    from: selection.from,
    to: selection.to,
    selectionAnchor: selection.from,
    selectionHead: selection.from + quotedText.length,
  };
}

// ── Tests ──────────────────────────────────────────────────────────

describe("applyInlineFormatting", () => {
  it("wraps selected text with bold markers (**)", () => {
    const doc = "hello world";
    const result = applyInlineFormatting(doc, { from: 6, to: 11 }, "**");
    expect(result.insert).toBe("**world**");
  });

  it("places cursor inside markers when nothing is selected", () => {
    const doc = "hello world";
    // Cursor at position 5 (no selection, from === to)
    const result = applyInlineFormatting(doc, { from: 5, to: 5 }, "**");
    expect(result.insert).toBe("****");
    // Cursor should be between the markers
    expect(result.selectionAnchor).toBe(7);
    expect(result.selectionHead).toBe(7);
  });

  it("selects the wrapped text after formatting", () => {
    const doc = "hello world";
    const result = applyInlineFormatting(doc, { from: 6, to: 11 }, "**");
    // Selection should cover "world" inside the markers
    expect(result.selectionAnchor).toBe(8); // after "**"
    expect(result.selectionHead).toBe(13); // before "**"
  });

  it("handles asymmetric markers (e.g. <u>...</u>)", () => {
    const doc = "hello world";
    const result = applyInlineFormatting(doc, { from: 0, to: 5 }, "<u>", "</u>");
    expect(result.insert).toBe("<u>hello</u>");
    // Selection covers "hello" between <u> and </u>
    expect(result.selectionAnchor).toBe(3); // after "<u>"
    expect(result.selectionHead).toBe(8); // before "</u>"
  });

  it("handles italic formatting (*)", () => {
    const doc = "emphasis";
    const result = applyInlineFormatting(doc, { from: 0, to: 8 }, "*");
    expect(result.insert).toBe("*emphasis*");
    expect(result.selectionAnchor).toBe(1);
    expect(result.selectionHead).toBe(9);
  });

  it("works at the beginning of the document", () => {
    const doc = "text";
    const result = applyInlineFormatting(doc, { from: 0, to: 4 }, "**");
    expect(result.from).toBe(0);
    expect(result.insert).toBe("**text**");
  });

  it("works with multi-word selections", () => {
    const doc = "one two three";
    const result = applyInlineFormatting(doc, { from: 0, to: 13 }, "**");
    expect(result.insert).toBe("**one two three**");
    expect(result.selectionAnchor).toBe(2);
    expect(result.selectionHead).toBe(15);
  });
});

describe("applyLinkFormatting", () => {
  it("wraps selected text in link syntax", () => {
    const doc = "click here";
    const result = applyLinkFormatting(doc, { from: 0, to: 10 });
    expect(result.insert).toBe("[click here](https://)");
  });

  it("selects the link text after formatting (existing selection)", () => {
    const doc = "click here";
    const result = applyLinkFormatting(doc, { from: 0, to: 10 });
    // Should select "click here" inside the brackets
    expect(result.selectionAnchor).toBe(1);
    expect(result.selectionHead).toBe(11);
  });

  it("inserts placeholder text when nothing is selected", () => {
    const doc = "hello world";
    const result = applyLinkFormatting(doc, { from: 5, to: 5 });
    expect(result.insert).toBe("[link text](https://)");
  });

  it("selects 'link text' placeholder when nothing was selected", () => {
    const doc = "hello world";
    const result = applyLinkFormatting(doc, { from: 5, to: 5 });
    // Should select "link text" inside the brackets
    expect(result.selectionAnchor).toBe(6); // 5 + 1 (after "[")
    expect(result.selectionHead).toBe(15); // 6 + "link text".length
  });
});

describe("applyQuoteFormatting", () => {
  it("prefixes a single line with '> '", () => {
    const doc = "hello world";
    const result = applyQuoteFormatting(doc, { from: 0, to: 11 });
    expect(result.insert).toBe("> hello world");
  });

  it("prefixes each line of a multi-line selection", () => {
    const doc = "line1\nline2\nline3";
    const result = applyQuoteFormatting(doc, { from: 0, to: 17 });
    expect(result.insert).toBe("> line1\n> line2\n> line3");
  });

  it("selects the entire quoted result", () => {
    const doc = "line1\nline2";
    const result = applyQuoteFormatting(doc, { from: 0, to: 11 });
    expect(result.selectionAnchor).toBe(0);
    expect(result.selectionHead).toBe("> line1\n> line2".length);
  });

  it("uses placeholder 'quote' when nothing is selected", () => {
    const doc = "some text";
    const result = applyQuoteFormatting(doc, { from: 4, to: 4 });
    expect(result.insert).toBe("> quote");
  });

  it("handles empty lines in selection", () => {
    const doc = "line1\n\nline3";
    const result = applyQuoteFormatting(doc, { from: 0, to: 12 });
    expect(result.insert).toBe("> line1\n> \n> line3");
  });
});

describe("edit/view toggle conditions", () => {
  /**
   * Mirrors the handleBlur logic from MarkdownCell.tsx:
   * Only exit edit mode if cell.source.trim() is non-empty.
   * This prevents empty cells from being "stuck" in view mode
   * with no content and no way to type.
   */
  function shouldExitEditOnBlur(source: string): boolean {
    return source.trim().length > 0;
  }

  it("exits edit mode when cell has content", () => {
    expect(shouldExitEditOnBlur("# Hello")).toBe(true);
  });

  it("stays in edit mode when cell is empty", () => {
    expect(shouldExitEditOnBlur("")).toBe(false);
  });

  it("stays in edit mode when cell is only whitespace", () => {
    expect(shouldExitEditOnBlur("   \n\t  ")).toBe(false);
  });

  it("exits edit mode for single character content", () => {
    expect(shouldExitEditOnBlur("a")).toBe(true);
  });

  /**
   * Mirrors the initial editing state:
   * const [editing, setEditing] = useState(cell.source === "");
   * Empty cells start in edit mode.
   */
  function shouldStartInEditMode(source: string): boolean {
    return source === "";
  }

  it("starts in edit mode for empty cells", () => {
    expect(shouldStartInEditMode("")).toBe(true);
  });

  it("starts in view mode for cells with content", () => {
    expect(shouldStartInEditMode("# Title")).toBe(false);
  });

  it("starts in view mode for whitespace-only cells (strict equality)", () => {
    // Note: this is intentionally strict equality (===), not .trim()
    // A cell with only spaces starts in view mode, which might be a quirk
    expect(shouldStartInEditMode("  ")).toBe(false);
  });
});
