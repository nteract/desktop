/** Check if a MIME type is a Vega or Vega-Lite variant (any version, both +json and .json). */
export function isVegaMimeType(mime: string): boolean {
  return /^application\/vnd\.vega(lite)?\.v\d/.test(mime);
}
