import { describe, expect, it } from "vitest";
import { rewriteMarkdownAssetRefs } from "../markdown-assets";

describe("rewriteMarkdownAssetRefs", () => {
  it("rewrites markdown attachment refs", () => {
    const result = rewriteMarkdownAssetRefs(
      "![plot](attachment:image.png)",
      { "attachment:image.png": "abc123" },
      4321,
    );

    expect(result).toBe("![plot](http://127.0.0.1:4321/blob/abc123)");
  });

  it("preserves markdown image titles", () => {
    const result = rewriteMarkdownAssetRefs(
      '![plot](images/foo.png "Diagram")',
      { "images/foo.png": "abc123" },
      4321,
    );

    expect(result).toBe('![plot](http://127.0.0.1:4321/blob/abc123 "Diagram")');
  });

  it("rewrites markdown image refs with parenthesized titles", () => {
    const result = rewriteMarkdownAssetRefs(
      "![plot](images/foo.png (Diagram))",
      { "images/foo.png": "abc123" },
      4321,
    );

    expect(result).toBe("![plot](http://127.0.0.1:4321/blob/abc123 (Diagram))");
  });

  it("rewrites reference-style image definitions", () => {
    const result = rewriteMarkdownAssetRefs(
      "![logo][img]\n\n[img]: images/logo.png 'Brand'",
      { "images/logo.png": "abc123" },
      4321,
    );

    expect(result).toBe(
      "![logo][img]\n\n[img]: http://127.0.0.1:4321/blob/abc123 'Brand'",
    );
  });

  it("rewrites inline html image refs", () => {
    const result = rewriteMarkdownAssetRefs(
      '<img src="images/foo.png" alt="diagram" />',
      { "images/foo.png": "abc123" },
      4321,
    );

    expect(result).toBe(
      '<img src="http://127.0.0.1:4321/blob/abc123" alt="diagram" />',
    );
  });

  it("rewrites unquoted inline html image refs", () => {
    const result = rewriteMarkdownAssetRefs(
      "<img src=images/foo.png alt=diagram />",
      { "images/foo.png": "abc123" },
      4321,
    );

    expect(result).toBe(
      "<img src=http://127.0.0.1:4321/blob/abc123 alt=diagram />",
    );
  });

  it("rewrites html image refs case-insensitively", () => {
    const result = rewriteMarkdownAssetRefs(
      '<IMG SRC="images/foo.png" ALT="diagram" />',
      { "images/foo.png": "abc123" },
      4321,
    );

    expect(result).toBe(
      '<IMG SRC="http://127.0.0.1:4321/blob/abc123" ALT="diagram" />',
    );
  });

  it("rewrites every matching occurrence", () => {
    const result = rewriteMarkdownAssetRefs(
      '![one](images/foo.png)\n<img src="images/foo.png" />',
      { "images/foo.png": "abc123" },
      4321,
    );

    expect(result).toBe(
      '![one](http://127.0.0.1:4321/blob/abc123)\n<img src="http://127.0.0.1:4321/blob/abc123" />',
    );
  });

  it("rewrites angle-bracketed markdown destinations with spaces", () => {
    const result = rewriteMarkdownAssetRefs(
      "![plot](<images/my plot.png>)",
      { "images/my plot.png": "abc123" },
      4321,
    );

    expect(result).toBe("![plot](http://127.0.0.1:4321/blob/abc123)");
  });

  it("leaves incomplete markdown image syntax unchanged", () => {
    const source = "![plot](images/foo.png";
    expect(
      rewriteMarkdownAssetRefs(source, { "images/foo.png": "abc123" }, 4321),
    ).toBe(source);
  });

  it("leaves malformed html image syntax unchanged", () => {
    const source = '<img src="images/foo.png alt="diagram" />';
    expect(
      rewriteMarkdownAssetRefs(source, { "images/foo.png": "abc123" }, 4321),
    ).toBe(source);
  });

  it("returns source unchanged without a blob port", () => {
    const source = "![plot](images/foo.png)";
    expect(
      rewriteMarkdownAssetRefs(source, { "images/foo.png": "abc123" }, null),
    ).toBe(source);
  });

  it("returns source unchanged without resolved assets", () => {
    const source = "![plot](images/foo.png)";
    expect(rewriteMarkdownAssetRefs(source, undefined, 4321)).toBe(source);
  });
});
