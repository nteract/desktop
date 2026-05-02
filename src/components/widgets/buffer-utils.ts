/**
 * Convert an ArrayBuffer, typed array, or DataView to a base64 string.
 */
export function arrayBufferToBase64(buffer: ArrayBuffer | Uint8Array | DataView): string {
  let bytes: Uint8Array;
  if (buffer instanceof Uint8Array) {
    bytes = buffer;
  } else if (buffer instanceof DataView) {
    bytes = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength);
  } else {
    bytes = new Uint8Array(buffer);
  }
  let binary = "";
  for (let i = 0; i < bytes.byteLength; i++) {
    binary += String.fromCharCode(bytes[i]);
  }
  return btoa(binary);
}

/**
 * Build a media src URL from a widget value that may be a string or binary data.
 *
 * Handles all value types sent by the Jupyter widget protocol:
 * - Binary data (DataView / ArrayBuffer / Uint8Array) from the ipywidgets
 *   binary-traitlet protocol (from_url(), from_file()), re-materialized as
 *   a DataView by the iframe's blob-URL resolver
 * - Data URLs, HTTP URLs, or absolute paths (passed through)
 * - Plain base64 strings (wrapped in a data URL)
 * - Falsy values (returns undefined)
 */
export function buildMediaSrc(
  value: string | ArrayBuffer | Uint8Array | DataView | null | undefined,
  mediaType: string,
  format: string,
): string | undefined {
  if (!value) return undefined;

  if (value instanceof ArrayBuffer || value instanceof Uint8Array || value instanceof DataView) {
    const base64 = arrayBufferToBase64(value);
    return `data:${mediaType}/${format};base64,${base64}`;
  }

  if (typeof value === "string") {
    if (
      value.startsWith("data:") ||
      value.startsWith("http://") ||
      value.startsWith("https://") ||
      value.startsWith("/")
    ) {
      return value;
    }
    return `data:${mediaType}/${format};base64,${value}`;
  }

  return undefined;
}
