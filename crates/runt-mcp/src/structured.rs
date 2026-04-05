//! Build structuredContent JSON for MCP output rendering.
//!
//! Tools that produce cell output (execute_cell, create_cell, set_cell, etc.)
//! return both text content (for LLM consumption) and structured JSON that
//! the output.html renderer can display.

use runtimed_client::resolved_output::{DataValue, Output, ResolvedCell};
use serde_json::{json, Value};

/// Check if a MIME type is a visualization spec (Plotly, Vega-Lite, Vega).
fn is_viz_mime(mime: &str) -> bool {
    mime == "application/vnd.plotly.v1+json"
        || (mime.starts_with("application/vnd.vegalite.v")
            && (mime.ends_with("+json") || mime.ends_with(".json")))
        || (mime.starts_with("application/vnd.vega.v")
            && !mime.starts_with("application/vnd.vegalite.")
            && (mime.ends_with("+json") || mime.ends_with(".json")))
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

            if let Some(ref output_data) = output.data {
                // Check if the output has a raster image that the MCP app can
                // render directly (png/jpeg/gif/webp via blob URL). When true,
                // we can safely skip text/html (which is often a massive base64
                // data URI for audio, or redundant chart HTML that duplicates
                // the image).
                let has_renderable_image = output_data.keys().any(|m| {
                    matches!(
                        m.as_str(),
                        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
                    )
                });

                for (mime, value) in output_data {
                    // Skip text/llm+plain — it's for LLM consumption, not the widget
                    if mime == "text/llm+plain" {
                        continue;
                    }

                    // Skip text/html when the MCP app can render a raster image
                    // instead. This avoids sending large base64 data URIs (audio
                    // HTML embeds) or redundant chart HTML alongside the image.
                    if mime == "text/html" && has_renderable_image {
                        continue;
                    }

                    // Skip viz JSON specs — the MCP app doesn't render them, and
                    // they're large (tens of KB). The app uses text/html or image
                    // fallbacks for display.
                    if is_viz_mime(mime) {
                        continue;
                    }

                    let json_value = match value {
                        DataValue::Binary(_) => {
                            // For binary data, use blob URL if available
                            blob_url_or_skip(output, mime)
                        }
                        DataValue::Json(v) => Some(v.clone()),
                        DataValue::Text(s) => Some(Value::String(s.clone())),
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
