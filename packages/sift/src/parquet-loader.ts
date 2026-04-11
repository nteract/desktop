/**
 * HuggingFace dataset URL resolution.
 *
 * Parquet reading is now handled by nteract-predicate WASM (load_parquet).
 */

/**
 * Resolve the Parquet file URL for a HuggingFace dataset.
 */
export async function resolveHuggingFaceParquetUrl(
  dataset: string,
  config = "default",
  split = "train",
): Promise<string> {
  const apiUrl = `https://datasets-server.huggingface.co/parquet?dataset=${encodeURIComponent(dataset)}`;
  const resp = await fetch(apiUrl);
  if (!resp.ok) {
    throw new Error(`HuggingFace API error: ${resp.status} ${resp.statusText}`);
  }
  const data = await resp.json();

  const files = data.parquet_files as Array<{
    config: string;
    split: string;
    url: string;
    filename: string;
    size: number;
  }>;

  if (!files || files.length === 0) {
    throw new Error(`No Parquet files found for ${dataset}`);
  }

  // Try exact config/split match, then split-only, then first available
  let match = files.find((f) => f.config === config && f.split === split);
  if (!match) match = files.find((f) => f.split === split);
  if (!match) match = files[0];

  return match.url;
}
