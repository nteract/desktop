//! Embedded renderer plugin assets for the MCP App.
//!
//! Heavy visualization renderers (plotly, vega, leaflet, markdown, sift) ship
//! inside the daemon binary via `include_bytes!`. The blob server serves them
//! at `GET /plugins/{name}`, wrapping `.js` responses in an IIFE for the MCP
//! App loader at request time (see `blob_server::wrap_for_mcp_app`).
//!
//! The canonical source directory is `apps/notebook/src/renderer-plugins/` -
//! the same raw CJS bundles the notebook Vite app loads via its
//! isolated-renderer virtual modules. Stable bundles (plotly, vega, leaflet,
//! markdown, isolated-renderer) are LFS-tracked; sift.js / sift.css are
//! rebuilt on every `cargo xtask wasm`. Sift's WASM blob comes straight from
//! `crates/sift-wasm/pkg/sift_wasm_bg.wasm`.
//!
//! ## Adding or removing a plugin asset
//!
//! 1. Source is `apps/notebook/src/renderer-plugins/{name}` (raw CJS for `.js`,
//!    CSS / WASM for their extensions).
//! 2. Update `EMBEDDED_PLUGINS` below - `include_bytes!` fails the build if a
//!    file is missing.
//! 3. `embedded_plugins_match_assets_dir` (tests) fails CI if the on-disk
//!    directory and `EMBEDDED_PLUGINS` drift apart for the stable set.

pub struct EmbeddedPlugin {
    pub name: &'static str,
    pub bytes: &'static [u8],
}

macro_rules! plugin {
    ($name:literal) => {
        EmbeddedPlugin {
            name: $name,
            bytes: include_bytes!(concat!(
                "../../../apps/notebook/src/renderer-plugins/",
                $name
            )),
        }
    };
}

/// Explicit manifest. Each entry is intentionally named so a stray file in
/// `renderer-plugins/` (backups, `.DS_Store`, scratch builds) can't end up
/// inside the daemon binary by accident.
///
/// `sift_wasm.wasm` is sourced from `crates/sift-wasm/pkg/` (wasm-pack's
/// canonical output dir) rather than the notebook renderer-plugin dir so the
/// daemon doesn't need a file copy step on every sift-wasm rebuild.
pub const EMBEDDED_PLUGINS: &[EmbeddedPlugin] = &[
    plugin!("markdown.js"),
    plugin!("markdown.css"),
    plugin!("plotly.js"),
    plugin!("vega.js"),
    plugin!("leaflet.js"),
    plugin!("leaflet.css"),
    plugin!("sift.js"),
    plugin!("sift.css"),
    EmbeddedPlugin {
        name: "sift_wasm.wasm",
        bytes: include_bytes!("../../sift-wasm/pkg/sift_wasm_bg.wasm"),
    },
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

/// Whether this plugin name is in the embedded manifest. Used to gate the
/// dev-mode filesystem path so `/plugins/{name}` exposes the same surface in
/// dev and release — otherwise a dev daemon could serve unrelated files that
/// happen to live under `apps/notebook/src/renderer-plugins/` (e.g.
/// `isolated-renderer.js`, which the Vite app loads directly).
pub fn is_embedded(name: &str) -> bool {
    EMBEDDED_PLUGINS.iter().any(|p| p.name == name)
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

    fn notebook_plugins_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apps/notebook/src/renderer-plugins")
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

    /// The notebook renderer-plugin directory and the JS/CSS half of
    /// `EMBEDDED_PLUGINS` must agree. sift_wasm.wasm lives under
    /// `crates/sift-wasm/pkg/` and is excluded from this drift check.
    #[test]
    fn embedded_plugins_match_assets_dir() {
        let dir = notebook_plugins_dir();
        let on_disk: HashSet<String> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", dir.display()))
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| !name.starts_with('.'))
            // isolated-renderer.{js,css} is loaded by the notebook Vite app
            // directly, not via `/plugins/`. Exclude it from the embedded
            // manifest drift check.
            .filter(|name| !name.starts_with("isolated-renderer."))
            .collect();

        let embedded: HashSet<String> = EMBEDDED_PLUGINS
            .iter()
            .map(|p| p.name.to_string())
            // WASM binary sits under crates/sift-wasm/pkg/.
            .filter(|name| name != "sift_wasm.wasm")
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
