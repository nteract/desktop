/**
 * MIME types safe for main-DOM rendering (no script execution risk).
 * Everything NOT in this set defaults to iframe isolation.
 *
 * This is a security allowlist — when adding new types, verify they
 * cannot execute arbitrary scripts or access the parent DOM.
 */
const MAIN_DOM_SAFE_TYPES = new Set([
  // Plain text with ANSI — no script risk
  "text/plain",
  // LaTeX — KaTeX renders safe static HTML
  "text/latex",
  // Raster images — <img> tags, no script risk
  "image/png",
  "image/jpeg",
  "image/gif",
  "image/webp",
  "image/bmp",
  // Structured data — tree viewer, no script risk
  "application/json",
]);

/** Check if a MIME type can safely render in the main DOM (no iframe needed). */
export function isSafeForMainDom(mimeType: string): boolean {
  if (MAIN_DOM_SAFE_TYPES.has(mimeType)) return true;
  // audio/* and video/* use native <audio>/<video> elements — safe
  if (mimeType.startsWith("audio/") || mimeType.startsWith("video/")) {
    return true;
  }
  return false;
}
