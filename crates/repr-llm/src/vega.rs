//! Vega visualization summarization.
//!
//! Vega specs are more complex than Vega-Lite (imperative transforms, custom marks).
//! We provide a simpler fallback summary: marks, data sources, and signals.

use serde_json::Value;

use crate::stats::extract_title;

/// Summarize a Vega spec into an LLM-friendly text representation.
pub fn summarize(spec: &Value) -> String {
    let mut lines = Vec::new();

    let title = spec.get("title").and_then(extract_title);

    // Extract mark types
    let mark_types = extract_mark_types(spec);
    // Extract data source names
    let data_names = extract_data_names(spec);

    // Header
    let header = match title {
        Some(t) => format!("Vega chart: \"{}\"", t),
        None => "Vega chart".to_string(),
    };
    lines.push(header);

    if !mark_types.is_empty() {
        lines.push(format!("Marks: [{}]", mark_types.join(", ")));
    }

    if !data_names.is_empty() {
        lines.push(format!("Data sources: [{}]", data_names.join(", ")));
    }

    // Extract signals (interactive parameters)
    let signals = extract_signal_names(spec);
    if !signals.is_empty() {
        lines.push(format!("Signals: [{}]", signals.join(", ")));
    }

    lines.join("\n")
}

/// Extract mark types from a Vega spec.
fn extract_mark_types(spec: &Value) -> Vec<String> {
    let marks = match spec.get("marks").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return Vec::new(),
    };

    marks
        .iter()
        .filter_map(|m| {
            m.get("type")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect()
}

/// Extract data source names from a Vega spec.
fn extract_data_names(spec: &Value) -> Vec<String> {
    let data = match spec.get("data").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return Vec::new(),
    };

    data.iter()
        .filter_map(|d| {
            d.get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect()
}

/// Extract signal names from a Vega spec.
fn extract_signal_names(spec: &Value) -> Vec<String> {
    let signals = match spec.get("signals").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return Vec::new(),
    };

    signals
        .iter()
        .filter_map(|s| {
            s.get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_basic_vega() {
        let spec = json!({
            "title": "My Vega Chart",
            "marks": [
                {"type": "rect"},
                {"type": "text"}
            ],
            "data": [
                {"name": "source"},
                {"name": "stats"}
            ]
        });
        let result = summarize(&spec);
        assert!(result.contains("Vega chart: \"My Vega Chart\""));
        assert!(result.contains("Marks: [rect, text]"));
        assert!(result.contains("Data sources: [source, stats]"));
    }

    #[test]
    fn test_with_signals() {
        let spec = json!({
            "marks": [{"type": "symbol"}],
            "data": [{"name": "points"}],
            "signals": [
                {"name": "hover", "value": null},
                {"name": "zoom", "value": 1}
            ]
        });
        let result = summarize(&spec);
        assert!(result.contains("Signals: [hover, zoom]"));
    }

    #[test]
    fn test_no_marks() {
        let spec = json!({
            "title": "Empty",
            "data": [{"name": "src"}]
        });
        let result = summarize(&spec);
        assert!(result.contains("Vega chart: \"Empty\""));
        assert!(!result.contains("Marks:"));
        assert!(result.contains("Data sources: [src]"));
    }

    #[test]
    fn test_minimal() {
        let spec = json!({});
        let result = summarize(&spec);
        assert_eq!(result, "Vega chart");
    }
}
