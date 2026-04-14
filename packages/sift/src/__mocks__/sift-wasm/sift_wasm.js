// Stub for when sift-wasm hasn't been built.
// Tests using the `data` prop work fine without WASM.
// The `url` prop's parquet path will fail gracefully (ensureModule rejects).
export default function init() {
  throw new Error("sift-wasm not built — run: cargo xtask wasm sift");
}
