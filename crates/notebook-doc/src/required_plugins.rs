//! MIME type → renderer plugin mapping.
//!
//! Shared by WASM (frontend plugin pre-warming), `runt-mcp` (structured
//! output metadata), and the daemon (future use). This is the Rust
//! equivalent of the TypeScript `pluginForMime()` in
//! `src/lib/renderer-plugins.ts`.

use std::collections::BTreeSet;

/// Map a MIME type to the renderer plugin name it requires, if any.
///
/// Returns `None` for MIME types that are rendered by built-in components
/// (e.g. `text/plain`, `text/html`, `image/png`).
pub fn plugin_for_mime(mime: &str) -> Option<&'static str> {
    match mime {
        "text/markdown" => Some("markdown"),
        "application/vnd.plotly.v1+json" => Some("plotly"),
        "application/geo+json" => Some("leaflet"),
        _ if is_vega_mime(mime) => Some("vega"),
        _ => None,
    }
}

/// Check if a MIME type is a Vega or Vega-Lite variant (any version).
///
/// Matches: `application/vnd.vega.v5+json`, `application/vnd.vegalite.v4+json`,
/// `application/vnd.vega.v3.json`, etc.
fn is_vega_mime(mime: &str) -> bool {
    let rest = match mime.strip_prefix("application/vnd.vega") {
        Some(r) => r,
        None => return false,
    };
    // After "application/vnd.vega", optionally consume "lite"
    let rest = rest.strip_prefix("lite").unwrap_or(rest);
    // Must be followed by ".v" and a digit
    rest.starts_with(".v") && rest.as_bytes().get(2).is_some_and(|b| b.is_ascii_digit())
}

/// Compute the deduplicated set of renderer plugin names required for
/// the given MIME types.
///
/// Accepts a flat list of MIME types (e.g. from an execution's
/// `mime_types` field in RuntimeStateDoc). Returns a sorted list of
/// unique plugin names.
pub fn compute_required_plugins(mime_types: &[String]) -> Vec<String> {
    let mut plugins = BTreeSet::new();
    for mime in mime_types {
        if let Some(p) = plugin_for_mime(mime) {
            plugins.insert(p);
        }
    }
    plugins.into_iter().map(String::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_mime_matches() {
        assert_eq!(plugin_for_mime("text/markdown"), Some("markdown"));
        assert_eq!(
            plugin_for_mime("application/vnd.plotly.v1+json"),
            Some("plotly")
        );
        assert_eq!(plugin_for_mime("application/geo+json"), Some("leaflet"));
    }

    #[test]
    fn test_vega_versions() {
        for v in 1..=7 {
            assert_eq!(
                plugin_for_mime(&format!("application/vnd.vega.v{}+json", v)),
                Some("vega"),
                "vega v{} should match",
                v
            );
            assert_eq!(
                plugin_for_mime(&format!("application/vnd.vegalite.v{}+json", v)),
                Some("vega"),
                "vegalite v{} should match",
                v
            );
        }
    }

    #[test]
    fn test_vega_dot_json_suffix() {
        assert_eq!(
            plugin_for_mime("application/vnd.vega.v5.json"),
            Some("vega")
        );
        assert_eq!(
            plugin_for_mime("application/vnd.vegalite.v4.json"),
            Some("vega")
        );
    }

    #[test]
    fn test_no_plugin_needed() {
        assert_eq!(plugin_for_mime("text/plain"), None);
        assert_eq!(plugin_for_mime("text/html"), None);
        assert_eq!(plugin_for_mime("image/png"), None);
        assert_eq!(plugin_for_mime("application/json"), None);
        assert_eq!(plugin_for_mime("image/svg+xml"), None);
    }

    #[test]
    fn test_vega_edge_cases() {
        // No version number
        assert_eq!(plugin_for_mime("application/vnd.vega+json"), None);
        // Missing dot before v
        assert_eq!(plugin_for_mime("application/vnd.vegav5+json"), None);
        // Not a digit after .v
        assert_eq!(plugin_for_mime("application/vnd.vega.vx+json"), None);
    }

    #[test]
    fn test_compute_required_plugins() {
        let mimes = vec![
            "text/plain".to_string(),
            "application/vnd.plotly.v1+json".to_string(),
            "text/markdown".to_string(),
            "image/png".to_string(),
        ];
        let plugins = compute_required_plugins(&mimes);
        assert_eq!(plugins, vec!["markdown", "plotly"]);
    }

    #[test]
    fn test_compute_deduplicates() {
        let mimes = vec![
            "application/vnd.vega.v5+json".to_string(),
            "application/vnd.vegalite.v4+json".to_string(),
        ];
        let plugins = compute_required_plugins(&mimes);
        assert_eq!(plugins, vec!["vega"]);
    }

    #[test]
    fn test_compute_empty() {
        let plugins = compute_required_plugins(&[]);
        assert!(plugins.is_empty());

        let mimes = vec!["text/plain".to_string(), "image/png".to_string()];
        let plugins = compute_required_plugins(&mimes);
        assert!(plugins.is_empty());
    }
}
