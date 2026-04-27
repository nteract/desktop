//! Embedded renderer plugin assets for the MCP App.
//!
//! Heavy visualization renderers (plotly, vega, leaflet, sift) are built
//! from source by `cargo xtask wasm` (which chains into
//! `cargo xtask renderer-plugins`) and embedded in the daemon binary via
//! `include_bytes!`. The blob server serves them directly from memory at
//! `GET /plugins/{name}`.
//!
//! The build artifacts under `crates/runt-mcp/assets/plugins/` are
//! gitignored — `cargo build -p runtimed` requires them to exist on disk
//! at compile time. CI builds them as a prerequisite step; local dev
//! runs `cargo xtask wasm` once after a fresh clone.
//!
//! ## Adding or removing a plugin asset
//!
//! 1. Build: `cargo xtask renderer-plugins` (writes to
//!    `crates/runt-mcp/assets/plugins/`).
//! 2. Update `EMBEDDED_PLUGINS` below — the `plugin!` macro's `include_bytes!`
//!    fails the build if the file is missing.
//! 3. `embedded_plugins_match_assets_dir` (in tests below) fails CI if the
//!    on-disk directory and `EMBEDDED_PLUGINS` drift apart.

pub struct EmbeddedPlugin {
    pub name: &'static str,
    pub bytes: &'static [u8],
}

macro_rules! plugin {
    ($name:literal) => {
        EmbeddedPlugin {
            name: $name,
            bytes: include_bytes!(concat!("../../runt-mcp/assets/plugins/", $name)),
        }
    };
}

/// Explicit manifest. Each entry is intentionally named — this prevents stray
/// files in `assets/plugins/` (backups, `.DS_Store`, scratch builds) from
/// accidentally shipping inside the daemon binary.
pub const EMBEDDED_PLUGINS: &[EmbeddedPlugin] = &[
    plugin!("markdown.js"),
    plugin!("markdown.css"),
    plugin!("plotly.js"),
    plugin!("vega.js"),
    plugin!("leaflet.js"),
    plugin!("leaflet.css"),
    plugin!("sift.js"),
    plugin!("sift.css"),
    plugin!("sift_wasm.wasm"),
];

// Compile-time guard: every embedded plugin should be at least a few KB.
// Files smaller than 1KB usually mean someone forgot to build them
// (`cargo xtask wasm`) or a build step copied a placeholder.
const _: () = {
    let mut i = 0;
    while i < EMBEDDED_PLUGINS.len() {
        assert!(
            EMBEDDED_PLUGINS[i].bytes.len() > 1024,
            "embedded plugin is too small — run `cargo xtask wasm`",
        );
        i += 1;
    }
};

/// Look up an embedded renderer plugin asset by filename.
/// Returns (bytes, content_type) or None.
pub fn get(name: &str) -> Option<(&'static [u8], &'static str)> {
    let plugin = EMBEDDED_PLUGINS.iter().find(|p| p.name == name)?;
    Some((plugin.bytes, content_type_for(name)?))
}

pub(crate) fn content_type_for(name: &str) -> Option<&'static str> {
    let (_, ext) = name.rsplit_once('.')?;
    match ext {
        "js" => Some("application/javascript; charset=utf-8"),
        "css" => Some("text/css; charset=utf-8"),
        "wasm" => Some("application/wasm"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn assets_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../runt-mcp/assets/plugins")
    }

    /// Every entry in `EMBEDDED_PLUGINS` must resolve to a known content type.
    /// If this fails, add the extension to `content_type_for`.
    #[test]
    fn every_embedded_plugin_has_a_content_type() {
        for plugin in EMBEDDED_PLUGINS {
            assert!(
                content_type_for(plugin.name).is_some(),
                "no content type for embedded plugin `{}` — add its extension to content_type_for",
                plugin.name,
            );
        }
    }

    /// The on-disk asset directory and `EMBEDDED_PLUGINS` must agree.
    ///
    /// - File on disk but missing from `EMBEDDED_PLUGINS`: the daemon won't
    ///   serve it (the bug fixed in #2051 where `sift.js`/`sift.css` 404'd).
    /// - Entry in `EMBEDDED_PLUGINS` but missing on disk: already caught by
    ///   `include_bytes!` at compile time, but checked here for symmetry.
    #[test]
    fn embedded_plugins_match_assets_dir() {
        let dir = assets_dir();
        let on_disk: HashSet<String> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", dir.display()))
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| !name.starts_with('.'))
            .collect();

        let embedded: HashSet<String> = EMBEDDED_PLUGINS
            .iter()
            .map(|p| p.name.to_string())
            .collect();

        let missing_from_manifest: Vec<&String> = on_disk.difference(&embedded).collect();
        let missing_from_disk: Vec<&String> = embedded.difference(&on_disk).collect();

        assert!(
            missing_from_manifest.is_empty() && missing_from_disk.is_empty(),
            "embedded_plugins.rs drift vs {}:\n\
             \n\
             Files on disk but not in EMBEDDED_PLUGINS: {:?}\n\
               → add `plugin!(\"name\")` to EMBEDDED_PLUGINS, or delete the file\n\
             \n\
             Entries in EMBEDDED_PLUGINS but not on disk: {:?}\n\
               → run `cargo xtask renderer-plugins`, or remove the entry",
            dir.display(),
            missing_from_manifest,
            missing_from_disk,
        );
    }

    #[test]
    fn get_returns_content_for_every_embedded_plugin() {
        for plugin in EMBEDDED_PLUGINS {
            let (bytes, content_type) =
                get(plugin.name).unwrap_or_else(|| panic!("get({}) returned None", plugin.name));
            assert_eq!(bytes.len(), plugin.bytes.len());
            assert!(!content_type.is_empty());
        }
    }

    #[test]
    fn get_returns_none_for_unknown_plugin() {
        assert!(get("nope.js").is_none());
        assert!(get("../etc/passwd").is_none());
    }
}
