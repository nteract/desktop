//! Embedded renderer plugin assets for the MCP App.
//!
//! Heavy visualization renderers (plotly, vega, leaflet) are committed to
//! the repo via Git LFS and embedded in the daemon binary via `include_bytes!`.
//! The blob server serves them directly from memory at `GET /plugins/{name}`.
//!
//! To update: run `cd apps/mcp-app && pnpm build:plugins`, then commit the
//! changed files. CI verifies the committed assets match `build:plugins` output.

// Compile-time guard: if LFS pointers weren't resolved, the plugin files
// are ~130 bytes (pointer stubs) instead of their real sizes. This catches
// builds where `git lfs pull` wasn't run.
const _: () = {
    const PLOTLY: &[u8] = include_bytes!("../../runt-mcp/assets/plugins/plotly.js");
    assert!(
        PLOTLY.len() > 1024,
        "plotly.js appears to be a Git LFS pointer — run `git lfs pull`"
    );
    const MARKDOWN: &[u8] = include_bytes!("../../runt-mcp/assets/plugins/markdown.js");
    assert!(
        MARKDOWN.len() > 1024,
        "markdown.js appears to be a Git LFS pointer — run `git lfs pull`"
    );
};

/// Look up an embedded renderer plugin asset by filename.
/// Returns (bytes, content_type) or None.
pub fn get(name: &str) -> Option<(&'static [u8], &'static str)> {
    match name {
        "markdown.js" => Some((
            include_bytes!("../../runt-mcp/assets/plugins/markdown.js"),
            "application/javascript; charset=utf-8",
        )),
        "markdown.css" => Some((
            include_bytes!("../../runt-mcp/assets/plugins/markdown.css"),
            "text/css; charset=utf-8",
        )),
        "plotly.js" => Some((
            include_bytes!("../../runt-mcp/assets/plugins/plotly.js"),
            "application/javascript; charset=utf-8",
        )),
        "vega.js" => Some((
            include_bytes!("../../runt-mcp/assets/plugins/vega.js"),
            "application/javascript; charset=utf-8",
        )),
        "leaflet.js" => Some((
            include_bytes!("../../runt-mcp/assets/plugins/leaflet.js"),
            "application/javascript; charset=utf-8",
        )),
        "leaflet.css" => Some((
            include_bytes!("../../runt-mcp/assets/plugins/leaflet.css"),
            "text/css; charset=utf-8",
        )),
        _ => None,
    }
}
