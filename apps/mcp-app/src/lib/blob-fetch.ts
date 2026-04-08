const cache = new Map<string, Promise<string>>();

/**
 * Check if a string looks like a blob URL from the daemon.
 * Matches: http://localhost:{port}/blob/{hash}
 */
export function isBlobUrl(value: unknown): value is string {
  if (typeof value !== "string") return false;
  return /^https?:\/\/(?:localhost|127\.0\.0\.1):\d+\/blob\//.test(value);
}

/**
 * Fetch text content from a blob URL, with caching.
 * Returns the original value if it's not a blob URL.
 * Evicts failed promises from cache so retries are possible.
 */
export async function fetchBlobText(value: string): Promise<string> {
  if (!isBlobUrl(value)) return value;

  const cached = cache.get(value);
  if (cached) return cached;

  const promise = fetch(value).then((r) => {
    if (!r.ok) throw new Error(`Blob fetch failed: ${r.status}`);
    return r.text();
  });

  cache.set(value, promise);

  // Evict on failure so future attempts can retry
  promise.catch(() => cache.delete(value));

  return promise;
}

/**
 * Fetch JSON content from a blob URL.
 */
export async function fetchBlobJson(value: string): Promise<unknown> {
  const text = await fetchBlobText(value);
  return JSON.parse(text);
}
