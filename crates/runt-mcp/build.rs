//! Ensure `assets/_output.html` exists so `include_str!` in `resources.rs`
//! never fails during `cargo check` or `cargo build` in a fresh worktree.
//!
//! The real asset is built by `apps/mcp-app/build-html.js` (via `cargo xtask
//! build` or `pnpm build` in `apps/mcp-app`). This build script only creates
//! a minimal placeholder when the file is missing.
//!
//! **Release builds refuse the placeholder** — shipping a stub instead of the
//! real output renderer would be a silent regression.

use std::path::Path;

fn main() {
    let asset = Path::new("assets/_output.html");

    // Re-run if the file is created or deleted externally.
    println!("cargo:rerun-if-changed=assets/_output.html");

    let is_release = std::env::var("PROFILE").unwrap_or_default() == "release";

    if !asset.exists() {
        if is_release {
            panic!(
                "assets/_output.html is missing — cannot build runt-mcp in release mode \
                 without the real output renderer. Run `cargo xtask build` first."
            );
        }
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
