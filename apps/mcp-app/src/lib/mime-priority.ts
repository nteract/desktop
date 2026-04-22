/**
 * MIME type priority for MCP App output rendering.
 * Higher priority types are preferred when multiple are available.
 */
export const MIME_PRIORITY: readonly string[] = [
  // Visualizations (highest priority — rich interactive content)
  "application/vnd.plotly.v1+json",
  "application/geo+json",
  // Data tables (parquet → Sift renderer)
  "application/vnd.apache.parquet",
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
 * Check if a MIME type is a Vega or Vega-Lite variant.
 */
export function isVegaMimeType(mime: string): boolean {
  return /^application\/vnd\.vega(lite)?\.v\d/.test(mime);
}

/**
 * Select the best MIME type to render from available data.
 * Returns null if nothing renderable is found.
 */
export function selectMimeType(data: Record<string, unknown>): string | null {
  const available = Object.keys(data).filter(
    (k) => data[k] != null && k !== "text/llm+plain",
  );

  // Check priority list first
  for (const mime of MIME_PRIORITY) {
    if (available.includes(mime)) return mime;
  }

  // Check for Vega/Vega-Lite variants (version-agnostic pattern match)
  const vegaMime = available.find(isVegaMimeType);
  if (vegaMime) return vegaMime;

  // Fallback: first available type
  return available[0] ?? null;
}
