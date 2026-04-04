//! Plotly visualization summarization.
//!
//! Walks a Plotly JSON spec and produces a compact text summary suitable for LLMs.

use serde_json::Value;

use crate::stats::{
    compute_stats, detect_trend, extract_numbers, extract_title, fmt_num, format_inline_pairs,
};

/// Summarize a Plotly spec into an LLM-friendly text representation.
pub fn summarize(spec: &Value) -> String {
    let mut lines = Vec::new();

    let layout = spec.get("layout");
    let title = layout.and_then(|l| l.get("title")).and_then(extract_title);

    let traces = match spec.get("data").and_then(|d| d.as_array()) {
        Some(arr) => arr,
        None => {
            // No data array — minimal fallback
            if let Some(t) = title {
                return format!("Plotly chart: \"{}\"", t);
            }
            return "Plotly chart (no data)".to_string();
        }
    };

    // Determine dominant chart type for header
    let trace_types: Vec<&str> = traces
        .iter()
        .filter_map(|t| t.get("type").and_then(|v| v.as_str()))
        .collect();
    let dominant_type = dominant_chart_type(&trace_types);

    // Header line
    let header = match title {
        Some(t) => format!("Plotly {} chart: \"{}\"", dominant_type, t),
        None => format!("Plotly {} chart", dominant_type),
    };
    lines.push(header);

    // Axis labels
    if let Some(l) = layout {
        let x_label = l
            .get("xaxis")
            .and_then(|a| a.get("title"))
            .and_then(extract_title);
        let y_label = l
            .get("yaxis")
            .and_then(|a| a.get("title"))
            .and_then(extract_title);
        if x_label.is_some() || y_label.is_some() {
            lines.push(format!(
                "Axes: x={}, y={}",
                x_label.unwrap_or("(unlabeled)"),
                y_label.unwrap_or("(unlabeled)")
            ));
        }
    }

    // Traces
    lines.push(format!("Traces: {}", traces.len()));
    for (i, trace) in traces.iter().enumerate() {
        let trace_type = trace
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let trace_name = trace.get("name").and_then(|v| v.as_str()).unwrap_or("");

        let display_type = classify_trace_type(trace);
        let summary = summarize_trace(trace, trace_type);

        let name_part = if trace_name.is_empty() {
            if traces.len() > 1 {
                format!("[trace {}]", i + 1)
            } else {
                String::new()
            }
        } else {
            format!("[{}]", trace_name)
        };

        if name_part.is_empty() {
            lines.push(format!("  ({}) {}", display_type, summary));
        } else {
            lines.push(format!("  {} ({}) {}", name_part, display_type, summary));
        }
    }

    lines.join("\n")
}

/// Classify a trace into a display type (e.g., "line" vs "scatter").
fn classify_trace_type(trace: &Value) -> &str {
    let trace_type = trace
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    match trace_type {
        "scatter" | "scattergl" => {
            let mode = trace.get("mode").and_then(|v| v.as_str()).unwrap_or("");
            if mode.contains("lines") {
                "line"
            } else {
                "scatter"
            }
        }
        "choroplethmapbox" => "choropleth",
        other => other,
    }
}

/// Determine the dominant chart type from trace types.
fn dominant_chart_type(trace_types: &[&str]) -> String {
    if trace_types.is_empty() {
        return "unknown".to_string();
    }
    if trace_types.len() == 1 {
        return trace_types[0].to_string();
    }

    // Count occurrences
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for &t in trace_types {
        if let Some(entry) = counts.iter_mut().find(|(name, _)| *name == t) {
            entry.1 += 1;
        } else {
            counts.push((t, 1));
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1));

    if counts.len() == 1 {
        counts[0].0.to_string()
    } else {
        // Multiple types — list them
        counts
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join("+")
    }
}

/// Summarize a single trace based on its type.
fn summarize_trace(trace: &Value, trace_type: &str) -> String {
    match trace_type {
        "bar" => summarize_bar(trace),
        "pie" => summarize_pie(trace),
        "scatter" | "scattergl" => summarize_scatter(trace),
        "heatmap" => summarize_heatmap(trace),
        "choropleth" | "choroplethmapbox" => summarize_choropleth(trace),
        _ => summarize_generic(trace, trace_type),
    }
}

/// Summarize a bar trace.
fn summarize_bar(trace: &Value) -> String {
    let x_arr = trace.get("x").and_then(|v| v.as_array());
    let y_arr = trace.get("y").and_then(|v| v.as_array());

    let (labels_arr, values_arr) = match (x_arr, y_arr) {
        (Some(x), Some(y)) => {
            // Determine which is categories and which is values
            // If x has strings, x=categories, y=values; otherwise y=categories, x=values
            let x_is_strings = x.first().is_some_and(|v| v.is_string());
            if x_is_strings {
                (x, y)
            } else {
                // Could be horizontal bar — check if y has strings
                let y_is_strings = y.first().is_some_and(|v| v.is_string());
                if y_is_strings {
                    (y, x)
                } else {
                    (x, y)
                }
            }
        }
        _ => return "no data".to_string(),
    };

    let labels: Vec<&str> = labels_arr.iter().filter_map(|v| v.as_str()).collect();
    let values = extract_numbers(values_arr);

    if labels.is_empty() || values.is_empty() {
        return format!("n={}", labels_arr.len().max(values_arr.len()));
    }

    let n = labels.len().min(values.len());
    if n <= 20 {
        format!("{{ {} }}", format_inline_pairs(&labels[..n], &values[..n]))
    } else {
        let stats = compute_stats(&values);
        match stats {
            Some(s) => format!(
                "n={}; range=[{}, {}], mean={}",
                n,
                fmt_num(s.min),
                fmt_num(s.max),
                fmt_num(s.mean)
            ),
            None => format!("n={}", n),
        }
    }
}

/// Summarize a pie trace.
fn summarize_pie(trace: &Value) -> String {
    let labels_arr = trace.get("labels").and_then(|v| v.as_array());
    let values_arr = trace.get("values").and_then(|v| v.as_array());

    let (labels_arr, values_arr) = match (labels_arr, values_arr) {
        (Some(l), Some(v)) => (l, v),
        _ => return "no data".to_string(),
    };

    let labels: Vec<&str> = labels_arr.iter().filter_map(|v| v.as_str()).collect();
    let values = extract_numbers(values_arr);

    if labels.is_empty() || values.is_empty() {
        return format!("n={}", labels_arr.len().max(values_arr.len()));
    }

    let total: f64 = values.iter().sum();
    let n = labels.len().min(values.len());

    if n <= 15 && total > 0.0 {
        let parts: Vec<String> = labels[..n]
            .iter()
            .zip(values[..n].iter())
            .map(|(l, v)| {
                let pct = v / total * 100.0;
                format!("{}: {} ({:.0}%)", l, fmt_num(*v), pct)
            })
            .collect();
        format!("{{ {} }}", parts.join(", "))
    } else {
        format!("n={} slices, total={}", n, fmt_num(total))
    }
}

/// Summarize a scatter/line trace.
fn summarize_scatter(trace: &Value) -> String {
    let x_arr = match trace.get("x").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return "no data".to_string(),
    };
    let y_arr = match trace.get("y").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return "no data".to_string(),
    };

    let x_vals = extract_numbers(x_arr);
    let y_vals = extract_numbers(y_arr);
    let n = x_vals.len().min(y_vals.len());

    if n == 0 {
        return "no data".to_string();
    }

    if n <= 15 {
        // Inline all points
        let pairs: Vec<String> = x_vals[..n]
            .iter()
            .zip(y_vals[..n].iter())
            .map(|(x, y)| format!("({}, {})", fmt_num(*x), fmt_num(*y)))
            .collect();
        format!("n={}: {}", n, pairs.join(", "))
    } else {
        // Stats + trend + endpoints
        let y_stats = compute_stats(&y_vals);
        let trend = detect_trend(&y_vals);
        let first_y = y_vals.first().copied().unwrap_or(0.0);
        let last_y = y_vals.last().copied().unwrap_or(0.0);
        let pct_change = if first_y.abs() > f64::EPSILON {
            (last_y - first_y) / first_y.abs() * 100.0
        } else {
            0.0
        };

        match y_stats {
            Some(s) => format!(
                "n={}; y: range=[{}, {}], mean={}; trend: {}; first={}, last={} ({:+.1}%)",
                n,
                fmt_num(s.min),
                fmt_num(s.max),
                fmt_num(s.mean),
                trend,
                fmt_num(first_y),
                fmt_num(last_y),
                pct_change
            ),
            None => format!("n={}", n),
        }
    }
}

/// Summarize a heatmap trace.
fn summarize_heatmap(trace: &Value) -> String {
    let z_arr = match trace.get("z").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return "no data".to_string(),
    };

    let rows = z_arr.len();
    let cols = z_arr
        .first()
        .and_then(|r| r.as_array())
        .map_or(0, |a| a.len());

    let x_labels: Option<Vec<&str>> = trace
        .get("x")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect());
    let y_labels: Option<Vec<&str>> = trace
        .get("y")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect());

    if rows <= 6 && cols <= 6 {
        // Full labeled matrix
        let mut matrix_lines = Vec::new();
        for (i, row) in z_arr.iter().enumerate() {
            if let Some(row_arr) = row.as_array() {
                let row_label = y_labels
                    .as_ref()
                    .and_then(|labels| labels.get(i).copied())
                    .unwrap_or("?");
                let cells: Vec<String> = row_arr
                    .iter()
                    .enumerate()
                    .map(|(j, v)| {
                        let col_label = x_labels
                            .as_ref()
                            .and_then(|labels| labels.get(j).copied())
                            .unwrap_or("?");
                        format!("{}:{}", col_label, fmt_num(v.as_f64().unwrap_or(0.0)))
                    })
                    .collect();
                matrix_lines.push(format!("  {}: {}", row_label, cells.join(", ")));
            }
        }
        format!("matrix {}x{}\n{}", rows, cols, matrix_lines.join("\n"))
    } else {
        // Shape + range
        let all_values: Vec<f64> = z_arr
            .iter()
            .filter_map(|row| row.as_array())
            .flat_map(|row| row.iter().filter_map(|v| v.as_f64()))
            .filter(|v| v.is_finite())
            .collect();

        match compute_stats(&all_values) {
            Some(s) => format!(
                "matrix {}x{}, range=[{}, {}], mean={}",
                rows,
                cols,
                fmt_num(s.min),
                fmt_num(s.max),
                fmt_num(s.mean)
            ),
            None => format!("matrix {}x{}", rows, cols),
        }
    }
}

/// Summarize a choropleth trace.
fn summarize_choropleth(trace: &Value) -> String {
    let locations = trace.get("locations").and_then(|v| v.as_array());
    let z_arr = trace
        .get("z")
        .and_then(|v| v.as_array())
        .or_else(|| trace.get("values").and_then(|v| v.as_array()));

    let (locations, z_arr) = match (locations, z_arr) {
        (Some(l), Some(z)) => (l, z),
        _ => return "no data".to_string(),
    };

    let loc_names: Vec<&str> = locations.iter().filter_map(|v| v.as_str()).collect();
    let z_vals = extract_numbers(z_arr);
    let n = loc_names.len().min(z_vals.len());

    if n == 0 {
        return "no data".to_string();
    }

    let stats = compute_stats(&z_vals);

    // Find top/bottom 3
    let mut indexed: Vec<(usize, f64)> = z_vals[..n].iter().copied().enumerate().collect();
    indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let top3: Vec<String> = indexed
        .iter()
        .take(3)
        .filter_map(|(i, v)| {
            loc_names
                .get(*i)
                .map(|name| format!("{}={}", name, fmt_num(*v)))
        })
        .collect();

    let bottom3: Vec<String> = indexed
        .iter()
        .rev()
        .take(3)
        .filter_map(|(i, v)| {
            loc_names
                .get(*i)
                .map(|name| format!("{}={}", name, fmt_num(*v)))
        })
        .collect();

    match stats {
        Some(s) => format!(
            "n={}, range=[{}, {}]; highest: {}; lowest: {}",
            n,
            fmt_num(s.min),
            fmt_num(s.max),
            top3.join(", "),
            bottom3.join(", ")
        ),
        None => format!("n={}", n),
    }
}

/// Generic fallback for unknown trace types.
fn summarize_generic(trace: &Value, trace_type: &str) -> String {
    // Try to determine data size from common fields
    let n = ["x", "y", "z", "values", "labels", "locations"]
        .iter()
        .filter_map(|key| trace.get(key)?.as_array().map(|a| a.len()))
        .max();

    match n {
        Some(n) => format!("{} data points", n),
        None => trace_type.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_bar_small() {
        let spec = json!({
            "data": [{
                "type": "bar",
                "x": ["Sun", "Sat", "Thur", "Fri"],
                "y": [21.41, 20.44, 17.68, 17.15]
            }],
            "layout": {"title": "Tips by Day"}
        });
        let result = summarize(&spec);
        assert!(result.contains("Plotly bar chart: \"Tips by Day\""));
        assert!(result.contains("Sun: 21.41"));
        assert!(result.contains("Fri: 17.15"));
    }

    #[test]
    fn test_bar_large() {
        let x: Vec<String> = (0..25).map(|i| format!("cat_{}", i)).collect();
        let y: Vec<f64> = (0..25).map(|i| i as f64 * 2.0).collect();
        let spec = json!({
            "data": [{"type": "bar", "x": x, "y": y}],
            "layout": {"title": "Many Bars"}
        });
        let result = summarize(&spec);
        assert!(result.contains("n=25"));
        assert!(result.contains("range="));
    }

    #[test]
    fn test_pie() {
        let spec = json!({
            "data": [{
                "type": "pie",
                "labels": ["Engineering", "Sales", "Marketing", "Support"],
                "values": [45, 28, 17, 10]
            }],
            "layout": {"title": "Department Size"}
        });
        let result = summarize(&spec);
        assert!(result.contains("Plotly pie chart"));
        assert!(result.contains("Engineering: 45 (45%)"));
        assert!(result.contains("Support: 10 (10%)"));
    }

    #[test]
    fn test_scatter_small() {
        let spec = json!({
            "data": [{
                "type": "scatter",
                "x": [1, 2, 3, 4, 5],
                "y": [2, 4, 6, 8, 10]
            }],
            "layout": {"title": "Linear"}
        });
        let result = summarize(&spec);
        assert!(result.contains("scatter"));
        assert!(result.contains("n=5"));
        assert!(result.contains("(1, 2)"));
        assert!(result.contains("(5, 10)"));
    }

    #[test]
    fn test_scatter_large() {
        let x: Vec<f64> = (0..50).map(|i| i as f64).collect();
        let y: Vec<f64> = (0..50).map(|i| i as f64 * 2.0 + 1.0).collect();
        let spec = json!({
            "data": [{
                "type": "scatter",
                "mode": "markers",
                "x": x,
                "y": y
            }],
            "layout": {"title": "Large Scatter"}
        });
        let result = summarize(&spec);
        assert!(result.contains("n=50"));
        assert!(result.contains("trend: increasing"));
    }

    #[test]
    fn test_line_chart() {
        let spec = json!({
            "data": [{
                "type": "scatter",
                "mode": "lines+markers",
                "x": [1, 2, 3],
                "y": [10, 20, 30]
            }],
            "layout": {"title": "Trend"}
        });
        let result = summarize(&spec);
        // Should classify as "line" not "scatter"
        assert!(result.contains("(line)"));
    }

    #[test]
    fn test_heatmap_small() {
        let spec = json!({
            "data": [{
                "type": "heatmap",
                "x": ["a", "b", "c"],
                "y": ["x", "y", "z"],
                "z": [[1, 2, 3], [4, 5, 6], [7, 8, 9]]
            }],
            "layout": {"title": "Correlation"}
        });
        let result = summarize(&spec);
        assert!(result.contains("matrix 3x3"));
        assert!(result.contains("a:1"));
    }

    #[test]
    fn test_heatmap_large() {
        let z: Vec<Vec<f64>> = (0..10)
            .map(|i| (0..10).map(|j| (i * 10 + j) as f64).collect())
            .collect();
        let spec = json!({
            "data": [{"type": "heatmap", "z": z}],
            "layout": {"title": "Big Heatmap"}
        });
        let result = summarize(&spec);
        assert!(result.contains("matrix 10x10"));
        assert!(result.contains("range="));
    }

    #[test]
    fn test_choropleth() {
        let spec = json!({
            "data": [{
                "type": "choropleth",
                "locations": ["USA", "CAN", "MEX", "BRA", "ARG"],
                "z": [100, 80, 60, 40, 20]
            }],
            "layout": {"title": "GDP"}
        });
        let result = summarize(&spec);
        assert!(result.contains("n=5"));
        assert!(result.contains("highest:"));
        assert!(result.contains("USA=100"));
    }

    #[test]
    fn test_no_title() {
        let spec = json!({
            "data": [{"type": "bar", "x": ["a", "b"], "y": [1, 2]}],
            "layout": {}
        });
        let result = summarize(&spec);
        assert!(result.contains("Plotly bar chart"));
        assert!(!result.contains("\"\""));
    }

    #[test]
    fn test_multi_trace() {
        let spec = json!({
            "data": [
                {"type": "scatter", "name": "Series A", "x": [1, 2], "y": [3, 4]},
                {"type": "scatter", "name": "Series B", "x": [1, 2], "y": [5, 6]}
            ],
            "layout": {"title": "Comparison"}
        });
        let result = summarize(&spec);
        assert!(result.contains("Traces: 2"));
        assert!(result.contains("[Series A]"));
        assert!(result.contains("[Series B]"));
    }

    #[test]
    fn test_fallback_unknown_type() {
        let spec = json!({
            "data": [{"type": "sunburst", "values": [1, 2, 3]}],
            "layout": {"title": "Sunburst"}
        });
        let result = summarize(&spec);
        assert!(result.contains("sunburst"));
        assert!(result.contains("3 data points"));
    }

    #[test]
    fn test_no_data() {
        let spec = json!({"layout": {"title": "Empty"}});
        let result = summarize(&spec);
        assert!(result.contains("Plotly chart: \"Empty\""));
    }

    #[test]
    fn test_title_as_object() {
        let spec = json!({
            "data": [{"type": "bar", "x": ["a"], "y": [1]}],
            "layout": {"title": {"text": "Object Title", "font": {"size": 14}}}
        });
        let result = summarize(&spec);
        assert!(result.contains("\"Object Title\""));
    }

    #[test]
    fn test_axis_labels() {
        let spec = json!({
            "data": [{"type": "scatter", "x": [1], "y": [2]}],
            "layout": {
                "title": "Test",
                "xaxis": {"title": "Time"},
                "yaxis": {"title": {"text": "Value"}}
            }
        });
        let result = summarize(&spec);
        assert!(result.contains("Axes: x=Time, y=Value"));
    }
}
