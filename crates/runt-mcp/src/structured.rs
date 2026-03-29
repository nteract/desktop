//! Build structuredContent JSON for MCP output rendering.
//!
//! Tools that produce cell output (execute_cell, create_cell, set_cell, etc.)
//! return both text content (for LLM consumption) and structured JSON that
//! the output.html renderer can display.

use runtimed_client::resolved_output::{DataValue, Output, ResolvedCell};
use serde_json::{json, Value};

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
                for (mime, value) in output_data {
                    let json_value = match value {
                        DataValue::Text(s) => Value::String(s.clone()),
                        DataValue::Binary(_) => {
                            // For binary data, use blob URL if available
                            if let Some(ref urls) = output.blob_urls {
                                if let Some(url) = urls.get(mime) {
                                    Value::String(url.clone())
                                } else {
                                    continue;
                                }
                            } else {
                                continue;
                            }
                        }
                        DataValue::Json(v) => v.clone(),
                    };
                    data.insert(mime.clone(), json_value);
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
