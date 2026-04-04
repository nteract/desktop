//! Structural summaries of large JSON values for LLM consumption.
//!
//! Produces compact descriptions of JSON structure (top-level keys, array lengths)
//! without including the actual data. Returns `None` for small JSON values that
//! are fine to pass through directly.

use serde_json::Value;

/// Minimum serialized size (bytes) before we bother summarizing.
const SIZE_THRESHOLD: usize = 2048;

/// Maximum summary length in characters.
const MAX_SUMMARY_LEN: usize = 500;

/// Produce a structural summary of a JSON value, or `None` if it's small enough
/// to pass through directly.
///
/// For objects: lists top-level keys with array lengths noted.
/// For arrays: notes length and summarizes first element structure.
pub fn summarize_json(value: &Value) -> Option<String> {
    // Estimate size without full serialization for obviously small values
    if is_obviously_small(value) {
        return None;
    }

    // For borderline cases, serialize to check actual size
    let serialized_len = serde_json::to_string(value).map(|s| s.len()).unwrap_or(0);

    if serialized_len < SIZE_THRESHOLD {
        return None;
    }

    let size_kb = serialized_len / 1024;

    let summary = match value {
        Value::Object(map) => {
            let keys_desc = summarize_object_keys(map);
            format!("JSON object ({size_kb} KB): {keys_desc}")
        }
        Value::Array(arr) => {
            let elem_desc = summarize_array(arr);
            format!("JSON array ({size_kb} KB): {elem_desc}")
        }
        // Scalars (string, number, bool, null) are never large enough to hit the threshold
        // in practice, but handle gracefully
        _ => format!("JSON value ({size_kb} KB)"),
    };

    Some(truncate_summary(&summary))
}

/// Quick check for values that are obviously small without serializing.
fn is_obviously_small(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => true,
        Value::String(s) => s.len() < SIZE_THRESHOLD,
        Value::Array(arr) => arr.is_empty(),
        Value::Object(map) => map.is_empty(),
    }
}

/// Summarize an object's top-level keys, noting array lengths.
fn summarize_object_keys(map: &serde_json::Map<String, Value>) -> String {
    let mut key_descs: Vec<String> = Vec::new();

    for (key, val) in map {
        let desc = match val {
            Value::Array(arr) => format!("{}({} items)", key, arr.len()),
            Value::Object(inner) => format!("{}({} keys)", key, inner.len()),
            _ => key.clone(),
        };
        key_descs.push(desc);
    }

    format!("keys=[{}]", key_descs.join(", "))
}

/// Summarize a top-level array: length and first element structure.
fn summarize_array(arr: &[Value]) -> String {
    let len = arr.len();
    let elem_desc = arr.first().map(describe_element).unwrap_or_default();

    if elem_desc.is_empty() {
        format!("{len} items")
    } else {
        format!("{len} items, each: {elem_desc}")
    }
}

/// Describe the structure of a single JSON value (for array element previews).
fn describe_element(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let keys: Vec<&str> = map.keys().map(|k| k.as_str()).collect();
            if keys.len() <= 8 {
                format!("{{{}}}", keys.join(", "))
            } else {
                let shown: Vec<&str> = keys.iter().take(6).copied().collect();
                format!("{{{}... +{} more}}", shown.join(", "), keys.len() - 6)
            }
        }
        Value::Array(arr) => format!("[{} items]", arr.len()),
        Value::String(_) => "string".to_string(),
        Value::Number(_) => "number".to_string(),
        Value::Bool(_) => "bool".to_string(),
        Value::Null => "null".to_string(),
    }
}

/// Truncate a summary to MAX_SUMMARY_LEN characters.
fn truncate_summary(s: &str) -> String {
    if s.chars().count() <= MAX_SUMMARY_LEN {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(MAX_SUMMARY_LEN - 3).collect();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_small_object_returns_none() {
        let val = json!({"name": "Alice", "age": 30});
        assert!(summarize_json(&val).is_none());
    }

    #[test]
    fn test_small_array_returns_none() {
        let val = json!([1, 2, 3]);
        assert!(summarize_json(&val).is_none());
    }

    #[test]
    fn test_empty_values_return_none() {
        assert!(summarize_json(&json!({})).is_none());
        assert!(summarize_json(&json!([])).is_none());
        assert!(summarize_json(&json!(null)).is_none());
        assert!(summarize_json(&json!(42)).is_none());
        assert!(summarize_json(&json!(true)).is_none());
    }

    #[test]
    fn test_large_object_with_arrays() {
        // Build a GeoJSON-like object > 2KB
        let features: Vec<Value> = (0..100)
            .map(|i| {
                json!({
                    "type": "Feature",
                    "properties": {"name": format!("Place {i}"), "pop": i * 1000},
                    "geometry": {"type": "Point", "coordinates": [i as f64, i as f64 * 2.0]}
                })
            })
            .collect();

        let val = json!({
            "type": "FeatureCollection",
            "features": features,
            "metadata": {"source": "test"}
        });

        let result = summarize_json(&val);
        assert!(result
            .as_ref()
            .is_some_and(|s| s.starts_with("JSON object")));
        assert!(result.as_ref().is_some_and(|s| s.contains("KB")));
        assert!(result
            .as_ref()
            .is_some_and(|s| s.contains("features(100 items)")));
        assert!(result
            .as_ref()
            .is_some_and(|s| s.contains("metadata(1 keys)")));
    }

    #[test]
    fn test_large_array_of_objects() {
        let items: Vec<Value> = (0..200)
            .map(|i| {
                json!({
                    "id": i,
                    "name": format!("Item {i}"),
                    "value": i as f64 * 1.5,
                    "active": i % 2 == 0
                })
            })
            .collect();

        let val = Value::Array(items);
        let result = summarize_json(&val);
        assert!(result.as_ref().is_some_and(|s| s.starts_with("JSON array")));
        assert!(result.as_ref().is_some_and(|s| s.contains("200 items")));
        assert!(result.as_ref().is_some_and(|s| s.contains("each:")));
        assert!(result.as_ref().is_some_and(|s| s.contains("id")));
    }

    #[test]
    fn test_large_array_of_scalars() {
        let items: Vec<Value> = (0..500)
            .map(|i| {
                json!(format!(
                    "long string value number {i} with padding to increase size xxxxxxxxxx"
                ))
            })
            .collect();
        let val = Value::Array(items);
        let result = summarize_json(&val);
        assert!(result.as_ref().is_some_and(|s| s.contains("500 items")));
        assert!(result.as_ref().is_some_and(|s| s.contains("each: string")));
    }

    #[test]
    fn test_object_with_nested_objects() {
        // Build an object with enough nested content to exceed threshold
        let mut map = serde_json::Map::new();
        for i in 0..20 {
            let mut inner = serde_json::Map::new();
            for j in 0..10 {
                inner.insert(
                    format!("field_{j}"),
                    json!(format!("value_{i}_{j}_padding_xxxxxxxxxx")),
                );
            }
            map.insert(format!("section_{i}"), Value::Object(inner));
        }
        let val = Value::Object(map);
        let result = summarize_json(&val);
        assert!(result
            .as_ref()
            .is_some_and(|s| s.starts_with("JSON object")));
        assert!(result.as_ref().is_some_and(|s| s.contains("10 keys")));
    }

    #[test]
    fn test_summary_truncation() {
        // Build an object with many keys to test truncation
        let mut map = serde_json::Map::new();
        for i in 0..100 {
            let items: Vec<Value> = (0..50)
                .map(|j| json!(format!("item_{i}_{j}_padding")))
                .collect();
            map.insert(
                format!("very_long_key_name_number_{i}"),
                Value::Array(items),
            );
        }
        let val = Value::Object(map);
        let result = summarize_json(&val);
        assert!(result.is_some_and(|s| s.chars().count() <= MAX_SUMMARY_LEN));
    }

    #[test]
    fn test_describe_element_many_keys() {
        let mut map = serde_json::Map::new();
        for i in 0..12 {
            map.insert(format!("key_{i}"), json!(i));
        }
        let desc = describe_element(&Value::Object(map));
        assert!(desc.contains("more"));
    }
}
