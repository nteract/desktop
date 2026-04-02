//! Ensure `assets/_output.html` exists so `include_str!` in `resources.rs`
//! never fails during `cargo check` or `cargo build` in a fresh worktree.
//!
//! The real asset is built by `apps/mcp-app/build-html.js` (via `cargo xtask
//! build` or `pnpm build` in `apps/mcp-app`). This build script only creates
//! a minimal placeholder when the file is missing.

use std::path::Path;

fn main() {
    let asset = Path::new("assets/_output.html");

    // Re-run if the file is created or deleted externally.
    println!("cargo:rerun-if-changed=assets/_output.html");

    if !asset.exists() {
        std::fs::create_dir_all("assets").ok();
        std::fs::write(
            asset,
            concat!(
                "<!DOCTYPE html><html><head><meta charset=\"utf-8\"></head>",
                "<body><p>Placeholder &mdash; run <code>cargo xtask build</code> ",
                "to generate the real output renderer.</p></body></html>",
            ),
        )
        .ok();
    }
}
