use std::path::{Path, PathBuf};

fn main() {
    // The renderer plugin bundles + sift wasm are gitignored build
    // artifacts produced by `cargo xtask wasm`. Check they exist before
    // letting `include_bytes!` blow up with a generic
    // "file not found in module path" error that doesn't tell anyone
    // what went wrong.
    let plugin_dir = Path::new("../runt-mcp/assets/plugins");
    let plugins = [
        "markdown.js",
        "markdown.css",
        "plotly.js",
        "vega.js",
        "leaflet.js",
        "leaflet.css",
        "sift.js",
        "sift.css",
        "sift_wasm.wasm",
    ];
    for file in plugins {
        let path = plugin_dir.join(file);
        println!("cargo:rerun-if-changed={}", path.display());
        if !path.exists() {
            panic!(
                "Missing renderer plugin asset: {}\n\n\
                 These artifacts are gitignored. Run `cargo xtask wasm` \
                 from the workspace root to (re)build them.",
                path.display(),
            );
        }
    }

    let out_dir = out_dir();
    build_metadata::emit_git_rerun_hints();
    build_metadata::write_git_hash(&out_dir);
}

fn out_dir() -> PathBuf {
    match std::env::var("OUT_DIR") {
        Ok(value) => PathBuf::from(value),
        Err(err) => panic!("OUT_DIR is required for build metadata: {err}"),
    }
}
