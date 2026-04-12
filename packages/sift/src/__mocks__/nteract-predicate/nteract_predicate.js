// Stub for CI where the WASM crate isn't built.
// Tests that use SiftTable with `data` prop don't need WASM.
export default function init() {
  throw new Error("nteract-predicate WASM not available in test environment");
}
