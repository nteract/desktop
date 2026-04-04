//! LLM-friendly text summaries of structured visualization specs.
//!
//! This crate produces compact, informative text representations of Plotly,
//! Vega-Lite, and Vega chart specs. These summaries are designed for LLM
//! consumption — compressing ~10k chars of JSON into ~300 chars of structured
//! text while preserving the information agents need to reason about charts.
//!
//! # Usage
//!
//! ```
//! use repr_llm::summarize_viz;
//! use serde_json::json;
//!
//! let spec = json!({
//!     "data": [{"type": "bar", "x": ["A", "B"], "y": [1, 2]}],
//!     "layout": {"title": "Example"}
//! });
//!
//! let summary = summarize_viz("application/vnd.plotly.v1+json", &spec);
//! assert!(summary.is_some());
//! ```

pub mod plotly;
pub(crate) mod stats;
pub mod vega;
pub mod vegalite;

use serde_json::Value;

/// Attempt to produce an LLM-friendly text summary from a visualization spec.
///
/// Returns `Some(summary)` if the MIME type is a recognized visualization format
/// (Plotly, Vega-Lite, or Vega), `None` otherwise.
pub fn summarize_viz(mime: &str, spec: &Value) -> Option<String> {
    if is_plotly_mime(mime) {
        Some(plotly::summarize(spec))
    } else if is_vegalite_mime(mime) {
        Some(vegalite::summarize(spec))
    } else if is_vega_mime(mime) {
        Some(vega::summarize(spec))
    } else {
        None
    }
}

/// Check if a MIME type is Plotly JSON.
fn is_plotly_mime(mime: &str) -> bool {
    mime == "application/vnd.plotly.v1+json"
}

/// Check if a MIME type is Vega-Lite JSON (any version).
fn is_vegalite_mime(mime: &str) -> bool {
    mime.starts_with("application/vnd.vegalite.v")
        && (mime.ends_with("+json") || mime.ends_with(".json"))
}

/// Check if a MIME type is Vega JSON (any version, excluding Vega-Lite).
fn is_vega_mime(mime: &str) -> bool {
    mime.starts_with("application/vnd.vega.v")
        && !mime.starts_with("application/vnd.vegalite.")
        && (mime.ends_with("+json") || mime.ends_with(".json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_plotly_mime() {
        assert!(is_plotly_mime("application/vnd.plotly.v1+json"));
        assert!(!is_plotly_mime("application/json"));
        assert!(!is_plotly_mime("application/vnd.vegalite.v5+json"));
    }

    #[test]
    fn test_vegalite_mime() {
        assert!(is_vegalite_mime("application/vnd.vegalite.v3+json"));
        assert!(is_vegalite_mime("application/vnd.vegalite.v4+json"));
        assert!(is_vegalite_mime("application/vnd.vegalite.v5+json"));
        assert!(is_vegalite_mime("application/vnd.vegalite.v5.json"));
        assert!(is_vegalite_mime("application/vnd.vegalite.v6+json"));
        assert!(!is_vegalite_mime("application/vnd.vega.v5+json"));
    }

    #[test]
    fn test_vega_mime() {
        assert!(is_vega_mime("application/vnd.vega.v4+json"));
        assert!(is_vega_mime("application/vnd.vega.v5+json"));
        assert!(is_vega_mime("application/vnd.vega.v5.json"));
        assert!(is_vega_mime("application/vnd.vega.v6+json"));
        assert!(!is_vega_mime("application/vnd.vegalite.v5+json"));
    }

    #[test]
    fn test_summarize_plotly() {
        let spec = json!({
            "data": [{"type": "bar", "x": ["a"], "y": [1]}],
            "layout": {"title": "Test"}
        });
        let result = summarize_viz("application/vnd.plotly.v1+json", &spec);
        assert!(result.is_some());
        assert!(result.as_ref().is_some_and(|s| s.contains("Plotly")));
    }

    #[test]
    fn test_summarize_vegalite() {
        let spec = json!({
            "mark": "bar",
            "encoding": {"x": {"field": "a", "type": "nominal"}}
        });
        let result = summarize_viz("application/vnd.vegalite.v5+json", &spec);
        assert!(result.is_some());
        assert!(result.as_ref().is_some_and(|s| s.contains("Vega-Lite")));
    }

    #[test]
    fn test_summarize_vega() {
        let spec = json!({
            "marks": [{"type": "rect"}],
            "data": [{"name": "table"}]
        });
        let result = summarize_viz("application/vnd.vega.v5+json", &spec);
        assert!(result.is_some());
        assert!(result.as_ref().is_some_and(|s| s.contains("Vega chart")));
    }

    #[test]
    fn test_summarize_unknown_mime() {
        let spec = json!({"key": "value"});
        assert!(summarize_viz("application/json", &spec).is_none());
        assert!(summarize_viz("text/plain", &spec).is_none());
    }
}
