//! Build structuredContent JSON for MCP output rendering.
//!
//! Tools that produce cell output (execute_cell, create_cell, set_cell, etc.)
//! return both text content (for LLM consumption) and structured JSON that
//! the output.html renderer can display.
//!
//! The manifest-based functions (`cell_structured_content_from_manifests`,
//! `manifest_output_to_structured`) read inline content directly from
//! ContentRef entries and emit blob URLs for blob-stored content. Zero blob
//! fetches — structured content is always compact.

use runtimed_client::output_resolver;
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

/// Build structuredContent JSON directly from manifest Values and blob URLs.
///
/// Unlike [`cell_structured_content`] which requires fully-resolved outputs,
/// this function reads inline content directly from ContentRef entries and
/// emits blob URLs for anything stored in the blob store. Zero blob fetches.
pub fn cell_structured_content_from_manifests(
    cell_id: &str,
    cell_type: &str,
    source: &str,
    output_manifests: &[serde_json::Value],
    execution_count: Option<i64>,
    status: &str,
    blob_base_url: &Option<String>,
) -> Value {
    let mut content = json!({
        "cell": {
            "cell_id": cell_id,
            "source": source,
            "cell_type": cell_type,
            "outputs": output_manifests.iter().map(|m| manifest_output_to_structured(m, blob_base_url)).collect::<Vec<_>>(),
            "execution_count": execution_count,
            "status": status,
        }
    });

    if let Some(base) = blob_base_url {
        content["blob_base_url"] = Value::String(base.clone());
    }

    content
}

/// Convert a single output manifest Value to structured JSON for the output renderer.
///
/// Reads inline content directly from ContentRef entries and emits blob URLs
/// for blob-stored content. No blob fetches are performed.
fn manifest_output_to_structured(manifest: &Value, blob_base_url: &Option<String>) -> Value {
    let output_type = manifest
        .get("output_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match output_type {
        "stream" => {
            let name = manifest.get("name").cloned().unwrap_or(Value::Null);
            // text is a ContentRef: {"inline": "..."} or {"blob": "hash", "size": N}
            let text = manifest
                .get("text")
                .and_then(|cr| resolve_text_content_ref(cr, blob_base_url))
                .unwrap_or(Value::Null);
            let mut out = json!({
                "output_type": "stream",
                "name": name,
                "text": text,
            });
            if let Some(preview) = manifest.get("llm_preview") {
                out["llm_preview"] = preview.clone();
            }
            out
        }
        "error" => {
            // traceback is a ContentRef (inline JSON string or blob), not a
            // raw string[]. Resolve it: parse inline JSON → array, or fall
            // back to a blob URL for rare oversized tracebacks.
            let traceback = manifest
                .get("traceback")
                .and_then(|cr| {
                    // Inline ContentRef: the value is a JSON-stringified array
                    // e.g. {"inline": "[\"line1\", \"line2\"]"}
                    if let Some(inline) = cr.get("inline").and_then(|v| v.as_str()) {
                        serde_json::from_str::<Value>(inline).ok()
                    } else if let Some(hash) = cr.get("blob").and_then(|v| v.as_str()) {
                        // Blob-stored traceback — return URL for renderer to fetch
                        blob_base_url
                            .as_ref()
                            .map(|base| Value::String(format!("{}/blob/{}", base, hash)))
                    } else if cr.is_array() {
                        // Legacy: already a plain JSON array
                        Some(cr.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or(Value::Null);
            let mut out = json!({
                "output_type": "error",
                "ename": manifest.get("ename").cloned().unwrap_or(Value::Null),
                "evalue": manifest.get("evalue").cloned().unwrap_or(Value::Null),
                "traceback": traceback,
            });
            if let Some(preview) = manifest.get("llm_preview") {
                out["llm_preview"] = preview.clone();
            }
            out
        }
        "display_data" | "execute_result" => {
            let mut data = serde_json::Map::new();

            if let Some(data_map) = manifest.get("data").and_then(|v| v.as_object()) {
                // Check for renderable raster images to skip redundant text/html
                let has_renderable_image = data_map.keys().any(|m| {
                    matches!(
                        m.as_str(),
                        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
                    )
                });

                for (mime, content_ref) in data_map {
                    // Skip text/html when a raster image exists — avoids large
                    // base64 data URIs or redundant chart HTML.
                    if mime == "text/html" && has_renderable_image {
                        continue;
                    }
                    if is_viz_mime(mime) || mime == "application/geo+json" {
                        // Viz specs: emit blob URL or inline data.
                        // The MCP App renderer fetches blob URLs on demand
                        // and parses inline JSON directly.
                        let meta = output_resolver::content_ref_meta(content_ref);
                        if let Some(hash) = meta.blob_hash {
                            if let Some(base) = blob_base_url.as_ref() {
                                data.insert(
                                    mime.clone(),
                                    Value::String(format!("{}/blob/{}", base, hash)),
                                );
                            }
                        } else if meta.is_inline {
                            // Small viz specs inlined in the CRDT — pass through
                            if let Some(inline) = content_ref.get("inline") {
                                data.insert(mime.clone(), inline.clone());
                            }
                        }
                        continue;
                    }

                    let meta = output_resolver::content_ref_meta(content_ref);

                    let json_value = if meta.is_inline {
                        // Inline content — extract the value directly
                        content_ref.get("inline").cloned()
                    } else if let Some(hash) = meta.blob_hash {
                        // Blob-stored content — emit blob URL
                        blob_base_url
                            .as_ref()
                            .map(|base| Value::String(format!("{}/blob/{}", base, hash)))
                    } else {
                        None
                    };

                    if let Some(jv) = json_value {
                        data.insert(mime.clone(), jv);
                    }
                }
            }

            // Synthesize text/llm+plain from viz specs that were skipped
            if data.is_empty() || !data.contains_key("text/llm+plain") {
                if let Some(data_map) = manifest.get("data").and_then(|v| v.as_object()) {
                    for (mime, content_ref) in data_map {
                        if is_viz_mime(mime) || mime == "application/geo+json" {
                            // Only try inline content (no blob fetches)
                            if let Some(inline) = content_ref.get("inline").and_then(|v| v.as_str())
                            {
                                if let Ok(spec) = serde_json::from_str::<Value>(inline) {
                                    if let Some(summary) = repr_llm::summarize_viz(mime, &spec) {
                                        data.insert(
                                            "text/llm+plain".to_string(),
                                            Value::String(summary),
                                        );
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            let mut result = json!({
                "output_type": output_type,
                "data": data,
            });

            if let Some(count) = manifest.get("execution_count").and_then(|v| v.as_i64()) {
                result["execution_count"] = json!(count);
            }

            result
        }
        _ => json!({"output_type": output_type}),
    }
}

/// Resolve a text ContentRef to a JSON value (inline text or blob URL).
fn resolve_text_content_ref(content_ref: &Value, blob_base_url: &Option<String>) -> Option<Value> {
    let meta = output_resolver::content_ref_meta(content_ref);
    if meta.is_inline {
        content_ref.get("inline").cloned()
    } else if let Some(hash) = meta.blob_hash {
        blob_base_url
            .as_ref()
            .map(|base| Value::String(format!("{}/blob/{}", base, hash)))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn inline_ref(content: &str) -> serde_json::Value {
        json!({"inline": content})
    }

    fn blob_ref(hash: &str, size: u64) -> serde_json::Value {
        json!({"blob": hash, "size": size})
    }

    #[test]
    fn structured_inline_content_passes_through() {
        let manifest = json!({
            "output_type": "display_data",
            "data": {
                "text/plain": inline_ref("hello"),
            },
        });
        let blob_base = Some("http://localhost:9999".to_string());
        let result = manifest_output_to_structured(&manifest, &blob_base);
        let Some(data) = result["data"].as_object() else {
            panic!("data should be an object");
        };
        assert_eq!(data["text/plain"], "hello");
    }

    #[test]
    fn structured_blob_becomes_url() {
        let manifest = json!({
            "output_type": "display_data",
            "data": {
                "image/png": blob_ref("abc123", 50_000),
            },
        });
        let blob_base = Some("http://localhost:9999".to_string());
        let result = manifest_output_to_structured(&manifest, &blob_base);
        let Some(data) = result["data"].as_object() else {
            panic!("data should be an object");
        };
        assert_eq!(data["image/png"], "http://localhost:9999/blob/abc123");
    }

    #[test]
    fn structured_no_blob_base_url_omits_blobs() {
        let manifest = json!({
            "output_type": "display_data",
            "data": {
                "image/png": blob_ref("abc123", 50_000),
                "text/plain": inline_ref("fallback"),
            },
        });
        let result = manifest_output_to_structured(&manifest, &None);
        let Some(data) = result["data"].as_object() else {
            panic!("data should be an object");
        };
        // Blob entry not included (no base URL to construct from)
        assert!(!data.contains_key("image/png"));
        // Inline entry still present
        assert_eq!(data["text/plain"], "fallback");
    }

    #[test]
    fn structured_viz_mime_inline_included() {
        // Inline viz specs are included for MCP App plugin rendering.
        // TODO: consider always blob-storing viz specs to avoid bloating
        // structured content visible to Claude Code. Currently small specs
        // (< 1KB) are inlined in the CRDT and passed through here.
        let manifest = json!({
            "output_type": "display_data",
            "data": {
                "application/vnd.plotly.v1+json": inline_ref("{\"data\": []}"),
                "text/plain": inline_ref("Figure()"),
            },
        });
        let blob_base = Some("http://localhost:9999".to_string());
        let result = manifest_output_to_structured(&manifest, &blob_base);
        let Some(data) = result["data"].as_object() else {
            panic!("data should be an object");
        };
        assert_eq!(data["application/vnd.plotly.v1+json"], "{\"data\": []}");
        assert_eq!(data["text/plain"], "Figure()");
    }

    #[test]
    fn structured_html_skipped_when_raster_image_exists() {
        let manifest = json!({
            "output_type": "display_data",
            "data": {
                "text/html": inline_ref("<img src='data:...' />"),
                "image/png": blob_ref("img_hash", 40_000),
                "text/plain": inline_ref("<Figure>"),
            },
        });
        let blob_base = Some("http://localhost:9999".to_string());
        let result = manifest_output_to_structured(&manifest, &blob_base);
        let Some(data) = result["data"].as_object() else {
            panic!("data should be an object");
        };
        assert!(!data.contains_key("text/html"));
        assert_eq!(data["image/png"], "http://localhost:9999/blob/img_hash");
    }

    #[test]
    fn structured_html_included_without_raster_image() {
        let manifest = json!({
            "output_type": "display_data",
            "data": {
                "text/html": inline_ref("<b>bold</b>"),
                "text/plain": inline_ref("bold"),
            },
        });
        let blob_base = Some("http://localhost:9999".to_string());
        let result = manifest_output_to_structured(&manifest, &blob_base);
        let Some(data) = result["data"].as_object() else {
            panic!("data should be an object");
        };
        assert_eq!(data["text/html"], "<b>bold</b>");
    }

    #[test]
    fn structured_llm_plain_included_as_fallback() {
        // text/llm+plain should NOT be filtered out in structured content
        let manifest = json!({
            "output_type": "display_data",
            "data": {
                "text/llm+plain": inline_ref("Summary of output"),
                "image/png": blob_ref("img", 10_000),
            },
        });
        let blob_base = Some("http://localhost:9999".to_string());
        let result = manifest_output_to_structured(&manifest, &blob_base);
        let Some(data) = result["data"].as_object() else {
            panic!("data should be an object");
        };
        assert_eq!(data["text/llm+plain"], "Summary of output");
    }

    #[test]
    fn structured_stream_inline() {
        let manifest = json!({
            "output_type": "stream",
            "name": "stdout",
            "text": inline_ref("hello"),
        });
        let result = manifest_output_to_structured(&manifest, &None);
        assert_eq!(result["output_type"], "stream");
        assert_eq!(result["name"], "stdout");
        assert_eq!(result["text"], "hello");
    }

    #[test]
    fn structured_stream_blob_text() {
        let manifest = json!({
            "output_type": "stream",
            "name": "stdout",
            "text": blob_ref("stream_hash", 5_000),
        });
        let blob_base = Some("http://localhost:9999".to_string());
        let result = manifest_output_to_structured(&manifest, &blob_base);
        assert_eq!(result["text"], "http://localhost:9999/blob/stream_hash");
    }

    #[test]
    fn structured_error_inline_traceback_parsed() {
        // Daemon stores traceback as ContentRef with JSON-stringified array
        let manifest = json!({
            "output_type": "error",
            "ename": "ValueError",
            "evalue": "bad",
            "traceback": inline_ref(r#"["line 1", "line 2"]"#),
        });
        let result = manifest_output_to_structured(&manifest, &None);
        assert_eq!(result["output_type"], "error");
        assert_eq!(result["ename"], "ValueError");
        // Traceback should be a parsed JSON array, not the raw ContentRef
        let Some(tb) = result["traceback"].as_array() else {
            panic!("traceback should be an array");
        };
        assert_eq!(tb.len(), 2);
        assert_eq!(tb[0], "line 1");
        assert_eq!(tb[1], "line 2");
    }

    #[test]
    fn structured_error_blob_traceback_becomes_url() {
        let manifest = json!({
            "output_type": "error",
            "ename": "RecursionError",
            "evalue": "maximum recursion depth exceeded",
            "traceback": blob_ref("tb_hash_123", 8_000),
        });
        let blob_base = Some("http://localhost:9999".to_string());
        let result = manifest_output_to_structured(&manifest, &blob_base);
        assert_eq!(
            result["traceback"],
            "http://localhost:9999/blob/tb_hash_123"
        );
    }

    #[test]
    fn structured_error_legacy_traceback_array() {
        // Legacy outputs may have a plain JSON array (not a ContentRef)
        let manifest = json!({
            "output_type": "error",
            "ename": "TypeError",
            "evalue": "oops",
            "traceback": ["line 1", "line 2"],
        });
        let result = manifest_output_to_structured(&manifest, &None);
        let Some(tb) = result["traceback"].as_array() else {
            panic!("legacy array should pass through");
        };
        assert_eq!(tb.len(), 2);
    }

    #[test]
    fn structured_execute_result_has_count() {
        let manifest = json!({
            "output_type": "execute_result",
            "data": {
                "text/plain": inline_ref("42"),
            },
            "execution_count": 7,
        });
        let result = manifest_output_to_structured(&manifest, &None);
        assert_eq!(result["execution_count"], 7);
    }

    #[test]
    fn structured_stream_includes_preview_when_blob() {
        let manifest = json!({
            "output_type": "stream",
            "name": "stdout",
            "text": blob_ref("stream_hash", 50_000),
            "llm_preview": {
                "head": "line 0\n",
                "tail": "line 99\n",
                "total_bytes": 50_000u64,
                "total_lines": 100u64,
            },
        });
        let blob_base = Some("http://localhost:9999".to_string());
        let result = manifest_output_to_structured(&manifest, &blob_base);
        assert_eq!(result["text"], "http://localhost:9999/blob/stream_hash");
        assert_eq!(result["llm_preview"]["total_lines"], 100);
        assert_eq!(result["llm_preview"]["head"], "line 0\n");
    }

    #[test]
    fn structured_error_includes_preview_when_blob() {
        let manifest = json!({
            "output_type": "error",
            "ename": "RecursionError",
            "evalue": "too deep",
            "traceback": blob_ref("tb_hash", 8_000),
            "llm_preview": {
                "last_frame": "RecursionError: too deep",
                "total_bytes": 8_000u64,
                "frames": 200u32,
            },
        });
        let blob_base = Some("http://localhost:9999".to_string());
        let result = manifest_output_to_structured(&manifest, &blob_base);
        assert_eq!(result["traceback"], "http://localhost:9999/blob/tb_hash");
        assert_eq!(result["llm_preview"]["frames"], 200);
        assert_eq!(
            result["llm_preview"]["last_frame"],
            "RecursionError: too deep"
        );
    }

    #[test]
    fn structured_stream_no_preview_for_inline() {
        let manifest = json!({
            "output_type": "stream",
            "name": "stdout",
            "text": inline_ref("hello"),
        });
        let result = manifest_output_to_structured(&manifest, &None);
        assert!(result.get("llm_preview").is_none());
    }

    #[test]
    fn cell_structured_content_wrapper() {
        let manifests = vec![json!({
            "output_type": "stream",
            "name": "stdout",
            "text": inline_ref("output"),
        })];
        let blob_base = Some("http://localhost:9999".to_string());
        let result = cell_structured_content_from_manifests(
            "cell-123",
            "code",
            "print('hello')",
            &manifests,
            Some(3),
            "done",
            &blob_base,
        );
        assert_eq!(result["cell"]["cell_id"], "cell-123");
        assert_eq!(result["cell"]["cell_type"], "code");
        assert_eq!(result["cell"]["source"], "print('hello')");
        assert_eq!(result["cell"]["execution_count"], 3);
        assert_eq!(result["cell"]["status"], "done");
        let Some(outputs) = result["cell"]["outputs"].as_array() else {
            panic!("outputs should be an array");
        };
        assert_eq!(outputs.len(), 1);
    }
}
