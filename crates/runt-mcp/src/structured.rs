//! Build structuredContent JSON for MCP output rendering.
//!
//! Tools that produce cell output (execute_cell, create_cell, set_cell, etc.)
//! return both text content (for LLM consumption) and structured JSON that
//! the output.html renderer can display.

use runtimed_client::resolved_output::{DataValue, Output, ResolvedCell};
use serde_json::{json, Value};

/// Maximum inline size (bytes) for a single MIME entry in structured content.
/// Entries larger than this are replaced with blob URLs or skipped.
const MAX_INLINE_BYTES: usize = 8 * 1024;

/// Check if a MIME type is a visualization spec (Plotly, Vega-Lite, Vega).
fn is_viz_mime(mime: &str) -> bool {
    mime == "application/vnd.plotly.v1+json"
        || (mime.starts_with("application/vnd.vegalite.v")
            && (mime.ends_with("+json") || mime.ends_with(".json")))
        || (mime.starts_with("application/vnd.vega.v")
            && !mime.starts_with("application/vnd.vegalite.")
            && (mime.ends_with("+json") || mime.ends_with(".json")))
}

/// Check if a JSON value exceeds the inline size limit when serialized.
fn json_exceeds_limit(v: &Value) -> bool {
    // Quick heuristic: serialize and check length.
    // For very large values, to_string is cheaper than to_string_pretty.
    serde_json::to_string(v).is_ok_and(|s| s.len() > MAX_INLINE_BYTES)
}

/// Return a blob URL for the given MIME type, or None to skip the entry.
fn blob_url_or_skip(output: &Output, mime: &str) -> Option<Value> {
    output
        .blob_urls
        .as_ref()
        .and_then(|urls| urls.get(mime))
        .map(|url| Value::String(url.clone()))
}

/// Build the structuredContent JSON for a resolved cell.
pub fn cell_structured_content(cell: &ResolvedCell, status: &str) -> Value {
    json!({
        "cell": {
            "cell_id": cell.id,
            "source": cell.source,
            "outputs": cell.outputs.iter().map(output_to_structured).collect::<Vec<_>>(),
            "execution_count": cell.execution_count,
            "status": status,
        }
    })
}

/// Convert a resolved Output to the JSON structure expected by the output renderer.
fn output_to_structured(output: &Output) -> Value {
    match output.output_type.as_str() {
        "stream" => {
            json!({
                "output_type": "stream",
                "name": output.name,
                "text": output.text,
            })
        }
        "error" => {
            json!({
                "output_type": "error",
                "ename": output.ename,
                "evalue": output.evalue,
                "traceback": output.traceback,
            })
        }
        "display_data" | "execute_result" => {
            let mut data = serde_json::Map::new();
            let has_blob_urls = output.blob_urls.is_some();

            if let Some(ref output_data) = output.data {
                // Check if the output has a richer primary MIME type, so we can
                // skip text/html (which often contains massive base64 data URIs
                // for audio or redundant chart HTML).
                let has_rich_primary = output_data.keys().any(|m| {
                    is_viz_mime(m)
                        || m.starts_with("image/")
                        || m.starts_with("audio/")
                        || m.starts_with("video/")
                });

                for (mime, value) in output_data {
                    // Skip text/llm+plain — it's for LLM consumption, not the widget
                    if mime == "text/llm+plain" {
                        continue;
                    }

                    // Skip text/html when richer types are available — it's a
                    // fallback representation that often contains inline base64
                    // data URIs (audio) or redundant chart HTML (plotly).
                    if mime == "text/html" && has_rich_primary {
                        continue;
                    }

                    let json_value = match value {
                        DataValue::Binary(_) => {
                            // For binary data, use blob URL if available
                            blob_url_or_skip(output, mime)
                        }
                        DataValue::Json(v) => {
                            // For viz JSON specs and large JSON, use blob URL
                            // instead of sending the full spec inline.
                            if is_viz_mime(mime) || json_exceeds_limit(v) {
                                blob_url_or_skip(output, mime)
                            } else {
                                Some(v.clone())
                            }
                        }
                        DataValue::Text(s) => {
                            // For large text (e.g. SVG), use blob URL if available
                            if s.len() > MAX_INLINE_BYTES && has_blob_urls {
                                blob_url_or_skip(output, mime)
                            } else {
                                Some(Value::String(s.clone()))
                            }
                        }
                    };

                    if let Some(jv) = json_value {
                        data.insert(mime.clone(), jv);
                    }
                }
            }

            let mut result = json!({
                "output_type": output.output_type,
                "data": data,
            });

            if let Some(count) = output.execution_count {
                result["execution_count"] = json!(count);
            }

            result
        }
        _ => json!({"output_type": output.output_type}),
    }
}
