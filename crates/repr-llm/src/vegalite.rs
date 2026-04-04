//! Vega-Lite visualization summarization.
//!
//! Walks a Vega-Lite JSON spec and produces a compact text summary suitable for LLMs.

use serde_json::Value;

use crate::stats::extract_title;

/// Summarize a Vega-Lite spec into an LLM-friendly text representation.
pub fn summarize(spec: &Value) -> String {
    // Check for composite views (layer, concat, etc.)
    if let Some(layers) = spec.get("layer").and_then(|v| v.as_array()) {
        return summarize_composite(spec, "layered", layers);
    }
    if let Some(charts) = spec.get("hconcat").and_then(|v| v.as_array()) {
        return summarize_composite(spec, "horizontal concat", charts);
    }
    if let Some(charts) = spec.get("vconcat").and_then(|v| v.as_array()) {
        return summarize_composite(spec, "vertical concat", charts);
    }
    if let Some(charts) = spec.get("concat").and_then(|v| v.as_array()) {
        return summarize_composite(spec, "concat", charts);
    }

    summarize_single(spec, 0)
}

/// Summarize a single (non-composite) Vega-Lite spec.
fn summarize_single(spec: &Value, indent: usize) -> String {
    let mut lines = Vec::new();
    let prefix = "  ".repeat(indent);

    // Mark type
    let mark = extract_mark(spec);

    // Title
    let title = spec.get("title").and_then(extract_title);

    // Header line
    let header = match (mark.as_deref(), title) {
        (Some(m), Some(t)) => format!("{}Vega-Lite {} chart: \"{}\"", prefix, m, t),
        (Some(m), None) => format!("{}Vega-Lite {} chart", prefix, m),
        (None, Some(t)) => format!("{}Vega-Lite chart: \"{}\"", prefix, t),
        (None, None) => format!("{}Vega-Lite chart", prefix),
    };
    lines.push(header);

    // Encoding
    if let Some(encoding) = spec.get("encoding").and_then(|v| v.as_object()) {
        let enc_parts: Vec<String> = ENCODING_CHANNELS
            .iter()
            .filter_map(|&channel| {
                let ch = encoding.get(channel)?;
                let field = ch.get("field").and_then(|v| v.as_str())?;
                let type_abbrev = ch
                    .get("type")
                    .and_then(|v| v.as_str())
                    .map(abbreviate_type)
                    .unwrap_or("?");
                let aggregate = ch
                    .get("aggregate")
                    .and_then(|v| v.as_str())
                    .map(|a| format!(" [{}]", a))
                    .unwrap_or_default();
                Some(format!(
                    "{}={}{} ({})",
                    channel, field, aggregate, type_abbrev
                ))
            })
            .collect();

        if !enc_parts.is_empty() {
            lines.push(format!("{}Encoding: {}", prefix, enc_parts.join(", ")));
        }
    }

    // Data summary
    if let Some(data) = spec.get("data") {
        if let Some(values) = data.get("values").and_then(|v| v.as_array()) {
            let n = values.len();
            let fields = extract_data_fields(values);
            if fields.is_empty() {
                lines.push(format!("{}Data: {} rows", prefix, n));
            } else {
                lines.push(format!(
                    "{}Data: {} rows, fields: [{}]",
                    prefix,
                    n,
                    fields.join(", ")
                ));
            }
        } else if let Some(url) = data.get("url").and_then(|v| v.as_str()) {
            lines.push(format!("{}Data: from URL {}", prefix, url));
        }
    }

    lines.join("\n")
}

/// Summarize a composite (layered/concatenated) Vega-Lite spec.
fn summarize_composite(spec: &Value, kind: &str, sub_specs: &[Value]) -> String {
    let mut lines = Vec::new();

    let title = spec.get("title").and_then(extract_title);
    let header = match title {
        Some(t) => format!(
            "Vega-Lite {} chart: \"{}\" ({} sub-charts)",
            kind,
            t,
            sub_specs.len()
        ),
        None => format!("Vega-Lite {} chart ({} sub-charts)", kind, sub_specs.len()),
    };
    lines.push(header);

    // Shared data
    if let Some(data) = spec.get("data") {
        if let Some(values) = data.get("values").and_then(|v| v.as_array()) {
            let fields = extract_data_fields(values);
            if fields.is_empty() {
                lines.push(format!("Shared data: {} rows", values.len()));
            } else {
                lines.push(format!(
                    "Shared data: {} rows, fields: [{}]",
                    values.len(),
                    fields.join(", ")
                ));
            }
        }
    }

    for (i, sub) in sub_specs.iter().enumerate() {
        lines.push(format!("  [{}]:", i + 1));
        lines.push(summarize_single(sub, 2));
    }

    lines.join("\n")
}

/// Extract the mark type from a Vega-Lite spec.
fn extract_mark(spec: &Value) -> Option<String> {
    let mark = spec.get("mark")?;
    if let Some(s) = mark.as_str() {
        return Some(s.to_string());
    }
    mark.get("type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Encoding channels to extract, in priority order.
const ENCODING_CHANNELS: &[&str] = &[
    "x", "y", "x2", "y2", "color", "size", "shape", "opacity", "detail", "row", "column", "facet",
    "text", "tooltip",
];

/// Abbreviate a Vega-Lite type string.
fn abbreviate_type(t: &str) -> &str {
    match t {
        "quantitative" => "Q",
        "nominal" => "N",
        "ordinal" => "O",
        "temporal" => "T",
        "geojson" => "Geo",
        _ => t,
    }
}

/// Extract field names from inline data values.
fn extract_data_fields(values: &[Value]) -> Vec<String> {
    let first = match values.first() {
        Some(v) => v,
        None => return Vec::new(),
    };

    match first.as_object() {
        Some(obj) => obj.keys().cloned().collect(),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_basic_bar() {
        let spec = json!({
            "mark": "bar",
            "encoding": {
                "x": {"field": "category", "type": "nominal"},
                "y": {"field": "value", "type": "quantitative"}
            },
            "title": "Sales by Category"
        });
        let result = summarize(&spec);
        assert!(result.contains("Vega-Lite bar chart: \"Sales by Category\""));
        assert!(result.contains("x=category (N)"));
        assert!(result.contains("y=value (Q)"));
    }

    #[test]
    fn test_inline_data() {
        let spec = json!({
            "mark": "point",
            "encoding": {
                "x": {"field": "x", "type": "quantitative"},
                "y": {"field": "y", "type": "quantitative"}
            },
            "data": {
                "values": [
                    {"x": 1, "y": 2},
                    {"x": 3, "y": 4},
                    {"x": 5, "y": 6}
                ]
            }
        });
        let result = summarize(&spec);
        assert!(result.contains("Data: 3 rows"));
        assert!(result.contains("fields:"));
    }

    #[test]
    fn test_url_data() {
        let spec = json!({
            "mark": "line",
            "encoding": {
                "x": {"field": "date", "type": "temporal"},
                "y": {"field": "price", "type": "quantitative"}
            },
            "data": {"url": "https://example.com/data.csv"}
        });
        let result = summarize(&spec);
        assert!(result.contains("Data: from URL"));
    }

    #[test]
    fn test_layered() {
        let spec = json!({
            "title": "Overlay",
            "data": {"values": [{"a": 1}, {"a": 2}]},
            "layer": [
                {"mark": "bar", "encoding": {"x": {"field": "a", "type": "quantitative"}}},
                {"mark": "line", "encoding": {"y": {"field": "a", "type": "quantitative"}}}
            ]
        });
        let result = summarize(&spec);
        assert!(result.contains("layered chart"));
        assert!(result.contains("2 sub-charts"));
        assert!(result.contains("bar"));
        assert!(result.contains("line"));
    }

    #[test]
    fn test_concat() {
        let spec = json!({
            "hconcat": [
                {"mark": "bar", "encoding": {"x": {"field": "a", "type": "nominal"}}},
                {"mark": "point", "encoding": {"x": {"field": "b", "type": "quantitative"}}}
            ]
        });
        let result = summarize(&spec);
        assert!(result.contains("horizontal concat"));
        assert!(result.contains("2 sub-charts"));
    }

    #[test]
    fn test_mark_as_object() {
        let spec = json!({
            "mark": {"type": "area", "opacity": 0.5},
            "encoding": {
                "x": {"field": "date", "type": "temporal"},
                "y": {"field": "value", "type": "quantitative"}
            }
        });
        let result = summarize(&spec);
        assert!(result.contains("area chart"));
    }

    #[test]
    fn test_color_encoding() {
        let spec = json!({
            "mark": "point",
            "encoding": {
                "x": {"field": "x", "type": "quantitative"},
                "y": {"field": "y", "type": "quantitative"},
                "color": {"field": "species", "type": "nominal"}
            }
        });
        let result = summarize(&spec);
        assert!(result.contains("color=species (N)"));
    }

    #[test]
    fn test_aggregate() {
        let spec = json!({
            "mark": "bar",
            "encoding": {
                "x": {"field": "category", "type": "nominal"},
                "y": {"field": "value", "type": "quantitative", "aggregate": "mean"}
            }
        });
        let result = summarize(&spec);
        assert!(result.contains("y=value [mean] (Q)"));
    }

    #[test]
    fn test_no_encoding() {
        let spec = json!({
            "mark": "rect",
            "title": "Empty"
        });
        let result = summarize(&spec);
        assert!(result.contains("Vega-Lite rect chart: \"Empty\""));
        assert!(!result.contains("Encoding"));
    }
}
