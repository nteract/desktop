/**
 * MIME type priority for MCP App output rendering.
 * Higher priority types are preferred when multiple are available.
 *
 * Phase 1: markdown, KaTeX, HTML, images, JSON, text.
 * Phase 2 (deferred): plotly, vega, leaflet, geo+json.
 */
export const MIME_PRIORITY: readonly string[] = [
  // Rich text
  "text/html",
  "text/markdown",
  "text/latex",
  // Images
  "image/svg+xml",
  "image/png",
  "image/jpeg",
  "image/gif",
  "image/webp",
  // Structured data
  "application/json",
  // Plain text (fallback)
  "text/plain",
];

/**
 * MIME types we know about but can't render yet.
 * Show text/plain fallback instead of the blob URL.
 */
const DEFERRED_MIMES = new Set([
  "application/vnd.plotly.v1+json",
  "application/geo+json",
]);

function isDeferredVizMime(mime: string): boolean {
  if (DEFERRED_MIMES.has(mime)) return true;
  if (mime.startsWith("application/vnd.vegalite.v") && mime.includes("+json")) return true;
  if (mime.startsWith("application/vnd.vega.v") && !mime.startsWith("application/vnd.vegalite.") && mime.includes("+json")) return true;
  return false;
}

/**
 * Select the best MIME type to render from available data.
 * Returns null if nothing renderable is found.
 */
export function selectMimeType(data: Record<string, unknown>): string | null {
  const available = Object.keys(data).filter(
    (k) => data[k] != null && k !== "text/llm+plain" && !isDeferredVizMime(k),
  );

  for (const mime of MIME_PRIORITY) {
    if (available.includes(mime)) return mime;
  }

  // Fallback: first available non-deferred type
  return available[0] ?? null;
}
