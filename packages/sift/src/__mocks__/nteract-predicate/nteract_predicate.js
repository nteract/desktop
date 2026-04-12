// Stub for when nteract-predicate WASM hasn't been built.
// Tests using the `data` prop work fine without WASM.
// The `url` prop's parquet path will fail gracefully (ensureModule rejects).
export default function init() {
  throw new Error("nteract-predicate WASM not built — run: cargo xtask wasm sift");
}
