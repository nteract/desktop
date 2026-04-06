/**
 * Default MIME type display priority for notebook outputs.
 *
 * Used by the WASM layer to select which MIME type to resolve when an
 * output bundle contains multiple representations. Higher priority
 * (earlier in the array) wins.
 */
export const DEFAULT_MIME_PRIORITY = [
  // Rich formats first
  "application/vnd.jupyter.widget-view+json",
  "application/vnd.plotly.v1+json",
  "application/vnd.vegalite.v6+json",
  "application/vnd.vegalite.v6.json",
  "application/vnd.vegalite.v5+json",
  "application/vnd.vegalite.v5.json",
  "application/vnd.vegalite.v4+json",
  "application/vnd.vegalite.v3+json",
  "application/vnd.vega.v6+json",
  "application/vnd.vega.v6.json",
  "application/vnd.vega.v5+json",
  "application/vnd.vega.v5.json",
  "application/vnd.vega.v4+json",
  "application/geo+json",
  // HTML, PDF, markdown, and LaTeX
  "text/html",
  "application/pdf",
  "text/markdown",
  "text/latex",
  "application/javascript",
  // Images
  "image/svg+xml",
  "image/png",
  "image/jpeg",
  "image/gif",
  "image/webp",
  "image/bmp",
  // Audio
  "audio/wav",
  "audio/mpeg",
  "audio/ogg",
  "audio/flac",
  "audio/webm",
  // Video
  "video/mp4",
  "video/webm",
  "video/ogg",
  // Structured data
  "application/json",
  // Plain text (fallback)
  "text/plain",
] as const;
