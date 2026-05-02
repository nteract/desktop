/**
 * Coerce a widget length value (from Python's `CUnicode` or `Int`) into a
 * valid CSS length.
 *
 * ipywidgets' `Image`, `Video`, etc. expose `width`/`height` as `CUnicode`.
 * `Image(width=64)` arrives as the bare string "64", which is not a valid
 * CSS length and causes browsers to fall back to the element's intrinsic
 * size — e.g. a 1x1 pixel for a 1x1 PNG. The canonical ipywidgets JS sets
 * the value as an HTML attribute (where bare numerics are interpreted as
 * pixels), so we do the same when building the React style prop.
 *
 * Already-unit-qualified strings (`"50%"`, `"64px"`, `"10rem"`) pass
 * through unchanged.
 */
export function toCssLength(value: string | number | null | undefined): string | undefined {
  if (value === null || value === undefined || value === "") return undefined;
  if (typeof value === "number") {
    return Number.isFinite(value) ? `${value}px` : undefined;
  }
  const trimmed = value.trim();
  if (trimmed === "") return undefined;
  // Bare numeric (integer or float) → pixels.
  if (/^-?\d+(\.\d+)?$/.test(trimmed)) {
    return `${trimmed}px`;
  }
  return trimmed;
}
