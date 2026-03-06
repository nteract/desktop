# Spike C: Custom WASM Bindings from `automerge = "0.7"`

> Eliminate the JS↔Rust Automerge version mismatch by compiling our own WASM module from the exact same `automerge` crate the daemon uses.

## Context

Phase 2 of the local-first migration is blocked by phantom cells appearing during JS↔Rust Automerge sync. The `@automerge/automerge@2.2.x` npm package bundles WASM compiled from an unknown version of `automerge-rs`. Our daemon uses `automerge = "0.7.4"` from crates.io. When the frontend (JS WASM) and the Tauri relay (Rust 0.7) exchange sync messages, phantom cells appear in the frontend's doc that don't exist in any Rust-side doc.

**Spike A confirmed:** Python bindings using the same Rust `NotebookSyncClient` (automerge 0.7 on both sides) work perfectly — create cell, sync, execute, output. No phantom cells.

**Conclusion:** The problem is the JS WASM Automerge build, not the relay architecture or the sync protocol. If we compile WASM from the same `automerge = "0.7"` crate, sync messages should be byte-compatible.

## Approach

Create a thin Rust crate (`crates/automerge-wasm-notebook`) that wraps the `NotebookDoc` operations and compiles to WASM via `wasm-pack`. The frontend imports this WASM module instead of `@automerge/automerge`. All Automerge operations happen through our WASM — same crate version, same serialization, guaranteed wire compatibility.

## Crate Design

```
crates/automerge-wasm-notebook/
├── Cargo.toml
├── src/
│   └── lib.rs          # wasm-bindgen exports
```

### Dependencies

```toml
[package]
name = "automerge-wasm-notebook"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
automerge = "0.7"           # Same version as runtimed
wasm-bindgen = "0.2"
js-sys = "0.3"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde-wasm-bindgen = "0.6"

[dependencies.web-sys]
version = "0.3"
features = ["console"]
```

### Exported API

The WASM module exposes a `NotebookHandle` class to JS:

```rust
#[wasm_bindgen]
pub struct NotebookHandle {
    doc: AutoCommit,
    sync_state: sync::State,
}

#[wasm_bindgen]
impl NotebookHandle {
    /// Load from saved doc bytes (from get_automerge_doc_bytes)
    #[wasm_bindgen(constructor)]
    pub fn load(bytes: &[u8]) -> Result<NotebookHandle, JsError>;

    /// Get all cells as JSON array
    pub fn get_cells(&self) -> Result<JsValue, JsError>;

    /// Add a cell at the given index
    pub fn add_cell(&mut self, index: usize, id: &str, cell_type: &str) -> Result<(), JsError>;

    /// Delete a cell by ID
    pub fn delete_cell(&mut self, cell_id: &str) -> Result<(), JsError>;

    /// Update cell source (uses Automerge Text CRDT update_text)
    pub fn update_source(&mut self, cell_id: &str, source: &str) -> Result<(), JsError>;

    /// Generate a sync message for the relay peer
    /// Returns None (undefined) if already in sync
    pub fn generate_sync_message(&mut self) -> Option<Vec<u8>>;

    /// Receive a sync message from the relay peer
    /// Returns the updated cells as JSON if the doc changed
    pub fn receive_sync_message(&mut self, message: &[u8]) -> Result<JsValue, JsError>;

    /// Export full doc as bytes (for debugging/persistence)
    pub fn save(&self) -> Vec<u8>;
}
```

This mirrors the operations `useAutomergeNotebook` currently performs, but all Automerge logic runs inside our WASM (same `automerge = "0.7"` crate). The JS side never touches Automerge directly.

## Frontend Integration

### Before (current broken approach)
```ts
import { next as Automerge } from "@automerge/automerge";  // Unknown automerge-rs version

const doc = Automerge.load(bytes);
const newDoc = Automerge.change(doc, d => {
  (d.cells as any).insertAt(0, { id: "...", ... });
});
const [syncState, msg] = Automerge.generateSyncMessage(newDoc, syncStateRef.current);
```

### After (Spike C)
```ts
import { NotebookHandle } from "automerge-wasm-notebook";  // Our automerge 0.7 WASM

const handle = NotebookHandle.load(bytes);
handle.add_cell(0, crypto.randomUUID(), "code");
const msg = handle.generate_sync_message();
if (msg) invoke("send_automerge_sync", { syncMessage: Array.from(msg) });
```

The `useAutomergeNotebook` hook replaces all `Automerge.*` calls with `NotebookHandle` method calls. The hook still owns the handle in a `useRef`, derives React state via `handle.get_cells()`, and syncs via the same Tauri relay.

## Build Pipeline

### Build

```bash
cd crates/automerge-wasm-notebook
wasm-pack build --target web --out-dir ../../apps/notebook/src/wasm/automerge-notebook
```

This produces:
- `automerge_wasm_notebook_bg.wasm` — the WASM binary
- `automerge_wasm_notebook.js` — JS glue code with `NotebookHandle` class
- `automerge_wasm_notebook.d.ts` — TypeScript types

### Vite Integration

The existing `vite-plugin-wasm` + `vite-plugin-top-level-await` plugins should handle the WASM import. If not, use the `?url` import pattern:

```ts
import init, { NotebookHandle } from "../wasm/automerge-notebook/automerge_wasm_notebook";
await init();  // Load WASM
```

### CI

Add to `cargo xtask build`:
```bash
wasm-pack build --target web crates/automerge-wasm-notebook --out-dir ../../apps/notebook/src/wasm/automerge-notebook
```

## Testing Strategy

### Step 1: Deno smoke test (fastest iteration)

Before touching the Tauri app, test from Deno which can load WASM directly:

```ts
// test-spike-c.ts — run with: deno run --allow-read test-spike-c.ts
import init, { NotebookHandle } from "./automerge_wasm_notebook.js";

await init(Deno.readFile("./automerge_wasm_notebook_bg.wasm"));

// Load fixture bytes from the Rust test
const fixtureHex = "856f4a83...";  // From notebook_doc::tests::export_fixture_bytes
const bytes = new Uint8Array(fixtureHex.match(/.{2}/g)!.map(b => parseInt(b, 16)));

const handle = NotebookHandle.load(bytes);
const cells = handle.get_cells();
console.log("Cells:", cells);  // Should show 1 cell with id "cell-1"

// Add a cell
handle.add_cell(1, "cell-2", "code");
handle.update_source("cell-2", "print('hello')");

const cells2 = handle.get_cells();
console.log("After add:", cells2);  // Should show 2 cells

// Generate sync message
const msg = handle.generate_sync_message();
console.log("Sync message:", msg ? `${msg.length} bytes` : "none");

// Create a second handle (simulating the relay) and sync
const handle2 = NotebookHandle.load(bytes);
if (msg) {
    const result = handle2.receive_sync_message(msg);
    console.log("Peer after sync:", handle2.get_cells());  // Should show 2 cells
}
```

### Step 2: Compat test with Rust relay

Write a Rust integration test that:
1. Creates a `NotebookDoc` (Rust), adds a cell, exports bytes
2. Loads those bytes in the WASM `NotebookHandle`
3. WASM adds a cell, generates sync message
4. Rust applies the sync message to its doc
5. Verify both docs have 2 cells with matching IDs

This directly tests the JS WASM → Rust sync path that's currently broken.

### Step 3: Integration in useAutomergeNotebook

Replace `@automerge/automerge` imports with `NotebookHandle`. The hook simplifies significantly — no more `Automerge.change()` callbacks, proxy methods, `RawString` handling, or `next` imports.

## Scope

### In scope
- [ ] Create `crates/automerge-wasm-notebook` crate
- [ ] Implement `NotebookHandle` with cell CRUD + sync operations
- [ ] `wasm-pack build` producing JS/TS/WASM output
- [ ] Deno smoke test proving sync roundtrip works
- [ ] Rust integration test proving WASM→Rust sync works
- [ ] Integrate into `useAutomergeNotebook` behind the existing feature flag
- [ ] Verify cell execution works end-to-end with the feature flag on

### Out of scope
- Removing `@automerge/automerge` npm dependency (cleanup, later)
- Removing the Tauri relay's Automerge doc (Phase 2D)
- Performance optimization
- Output handling changes

## Risk Assessment

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| `wasm-pack` build issues with `automerge = "0.7"` | Low | The crate is pure Rust, no C deps |
| WASM bundle too large | Medium | `automerge` is ~500KB uncompressed WASM; with wasm-opt and gzip should be <200KB |
| Vite can't load our custom WASM | Low | We already have `vite-plugin-wasm` working for `@automerge/automerge` |
| Sync still doesn't work | Low | The Python bindings prove Rust 0.7 ↔ Rust 0.7 sync works; WASM is the same code |
| `NotebookDoc` operations need to be duplicated | Medium | We can import from `runtimed` crate or extract shared operations |

## Success Criteria

1. Deno test: create cell in WASM handle, generate sync message, apply in second handle — cells match
2. Rust test: WASM-generated sync message applied to Rust `AutoCommit` — cells match
3. Runtime test: feature flag on, type in cell, Shift+Enter, see output — no "Cell not found"

## Relationship to Phase 2

This spike replaces Sub-PR 2A's `@automerge/automerge` dependency with our own WASM build. Sub-PRs 2B (relay infrastructure) and 2C (hook) remain largely the same — the hook just calls `NotebookHandle` methods instead of `Automerge.*` functions. The relay is unchanged.

If this works, we update the Phase 2 plan to use `automerge-wasm-notebook` and unblock Sub-PR 2C.