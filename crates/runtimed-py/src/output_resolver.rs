//! Output resolution for converting blob hashes and JSON to Output objects.
//!
//! This module provides standalone async functions for resolving outputs,
//! used by both Session (during execution) and Cell (when fetching from Automerge).

use std::collections::HashMap;
use std::path::PathBuf;

use base64::Engine as _;

use crate::output::Output;

/// Check if a string looks like a blob hash (64 hex characters).
fn is_blob_hash(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Check if a MIME type represents binary content.
///
/// Binary MIME types are stored as raw bytes in the blob store (decoded
/// from Jupyter's base64 wire format). When resolving, we read the raw
/// bytes and base64-encode them back to a string for the Output struct.
///
/// Must match the Rust `is_binary_mime` in `output_store.rs` and the
/// TypeScript `isBinaryMime` in `manifest-resolution.ts`.
fn is_binary_mime(mime: &str) -> bool {
    if mime.starts_with("image/") {
        // SVG is plain XML text in Jupyter, not base64-encoded binary.
        return !mime.ends_with("+xml");
    }
    if mime.starts_with("audio/") || mime.starts_with("video/") {
        return true;
    }

    // application/* is binary by default, with carve-outs for text-like formats.
    if let Some(subtype) = mime.strip_prefix("application/") {
        let is_text = subtype == "json"
            || subtype == "javascript"
            || subtype == "ecmascript"
            || subtype == "xml"
            || subtype == "xhtml+xml"
            || subtype == "mathml+xml"
            || subtype == "sql"
            || subtype == "graphql"
            || subtype == "x-latex"
            || subtype == "x-tex"
            || subtype.ends_with("+json")
            || subtype.ends_with("+xml");
        return !is_text;
    }

    false
}

/// Resolve an output string to an Output.
///
/// The output string can be:
/// - Raw JSON for backward compatibility
/// - A blob hash (64-char hex SHA-256) pointing to an output manifest
///
/// When output_type is None, attempts to extract it from the JSON/manifest.
pub async fn resolve_output_string(
    output_str: &str,
    blob_base_url: &Option<String>,
    blob_store_path: &Option<PathBuf>,
) -> Option<Output> {
    // Try to parse as raw JSON first
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(output_str) {
        // Extract output_type from JSON
        let output_type = parsed.get("output_type")?.as_str()?;
        return output_from_json(output_type, &parsed);
    }

    // If it looks like a blob hash, try to resolve it
    if is_blob_hash(output_str) {
        log::debug!("[output_resolver] Detected blob hash: {}", output_str);

        // First try: read directly from disk (most reliable).
        // Manifests are always JSON text, so read_to_string is correct here.
        if let Some(store_path) = blob_store_path {
            let prefix = &output_str[..2];
            let rest = &output_str[2..];
            let blob_path = store_path.join(prefix).join(rest);
            log::debug!("[output_resolver] Trying blob path: {:?}", blob_path);

            if let Ok(contents) = tokio::fs::read_to_string(&blob_path).await {
                log::debug!(
                    "[output_resolver] Read blob file, contents len: {}",
                    contents.len()
                );
                if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&contents) {
                    // Extract output_type from manifest
                    if let Some(output_type) = manifest.get("output_type").and_then(|v| v.as_str())
                    {
                        return output_from_manifest(
                            output_type,
                            &manifest,
                            blob_base_url,
                            blob_store_path,
                        )
                        .await;
                    }
                }
            }
        }

        // Second try: fetch from blob server
        if let Some(base_url) = blob_base_url {
            let url = format!("{}/blob/{}", base_url, output_str);
            if let Ok(response) = reqwest::get(&url).await {
                if response.status().is_success() {
                    if let Ok(manifest) = response.json::<serde_json::Value>().await {
                        if let Some(output_type) =
                            manifest.get("output_type").and_then(|v| v.as_str())
                        {
                            return output_from_manifest(
                                output_type,
                                &manifest,
                                blob_base_url,
                                blob_store_path,
                            )
                            .await;
                        }
                    }
                }
            }
        }
    }

    // Unable to resolve - return a fallback error output to preserve visibility
    log::debug!(
        "[output_resolver] Failed to resolve output string: {}",
        &output_str[..output_str.len().min(100)]
    );
    Some(Output::stream(
        "stderr",
        &format!(
            "Failed to resolve output: {}",
            &output_str[..output_str.len().min(64)]
        ),
    ))
}

/// Resolve an output with a known output_type.
///
/// Used during execution when the output_type is known from the broadcast message.
pub async fn resolve_output_with_type(
    output_type: &str,
    output_json: &str,
    blob_base_url: &Option<String>,
    blob_store_path: &Option<PathBuf>,
) -> Option<Output> {
    // Try to parse output_json directly first
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(output_json) {
        return output_from_json(output_type, &parsed);
    }

    // If it looks like a blob hash (64 hex chars), try to resolve it
    if is_blob_hash(output_json) {
        log::debug!("[output_resolver] Detected blob hash: {}", output_json);

        // First try: read directly from disk (most reliable).
        // Manifests are always JSON text, so read_to_string is correct here.
        if let Some(store_path) = blob_store_path {
            let prefix = &output_json[..2];
            let rest = &output_json[2..];
            let blob_path = store_path.join(prefix).join(rest);
            log::debug!("[output_resolver] Trying blob path: {:?}", blob_path);

            if let Ok(contents) = tokio::fs::read_to_string(&blob_path).await {
                log::debug!(
                    "[output_resolver] Read blob file, contents len: {}",
                    contents.len()
                );
                if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&contents) {
                    return output_from_manifest(
                        output_type,
                        &manifest,
                        blob_base_url,
                        blob_store_path,
                    )
                    .await;
                }
            }
        }

        // Second try: fetch from blob server
        if let Some(base_url) = blob_base_url {
            let url = format!("{}/blob/{}", base_url, output_json);
            if let Ok(response) = reqwest::get(&url).await {
                if response.status().is_success() {
                    if let Ok(manifest) = response.json::<serde_json::Value>().await {
                        return output_from_manifest(
                            output_type,
                            &manifest,
                            blob_base_url,
                            blob_store_path,
                        )
                        .await;
                    }
                }
            }
        }
    }

    // Fallback: create an error output to preserve failure semantics
    if output_type == "error" {
        Some(Output::error(
            "OutputParseError",
            &format!("Failed to parse error output: {}", output_json),
            vec![],
        ))
    } else {
        Some(Output::stream(
            "stderr",
            &format!("Failed to parse output: {}", output_json),
        ))
    }
}

/// Convert a parsed JSON value to an Output.
pub fn output_from_json(output_type: &str, json: &serde_json::Value) -> Option<Output> {
    match output_type {
        "stream" => {
            let name = json.get("name")?.as_str()?;
            let text = json.get("text")?.as_str()?;
            Some(Output::stream(name, text))
        }
        "display_data" => {
            let data = json.get("data")?.as_object()?;
            let mut output_data = HashMap::new();
            for (key, value) in data {
                if let Some(s) = value.as_str() {
                    output_data.insert(key.clone(), s.to_string());
                } else {
                    output_data.insert(key.clone(), value.to_string());
                }
            }
            Some(Output::display_data(output_data))
        }
        "execute_result" => {
            let data = json.get("data")?.as_object()?;
            let execution_count = json.get("execution_count")?.as_i64()?;
            let mut output_data = HashMap::new();
            for (key, value) in data {
                if let Some(s) = value.as_str() {
                    output_data.insert(key.clone(), s.to_string());
                } else {
                    output_data.insert(key.clone(), value.to_string());
                }
            }
            Some(Output::execute_result(output_data, execution_count))
        }
        "error" => {
            let ename = json.get("ename")?.as_str()?.to_string();
            let evalue = json.get("evalue")?.as_str()?.to_string();
            let traceback = json
                .get("traceback")?
                .as_array()?
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            Some(Output::error(&ename, &evalue, traceback))
        }
        _ => None,
    }
}

/// Convert a blob manifest to an Output.
///
/// The manifest has a format like:
/// {"output_type": "stream", "name": "stdout", "text": {"inline": "..."}}
///
/// For display_data/execute_result, each MIME type's content ref is resolved
/// according to its type: binary MIME types read raw bytes from the blob store
/// and base64-encode them; text MIME types read as UTF-8 strings.
pub async fn output_from_manifest(
    output_type: &str,
    manifest: &serde_json::Value,
    blob_base_url: &Option<String>,
    blob_store_path: &Option<PathBuf>,
) -> Option<Output> {
    match output_type {
        "stream" => {
            let name = manifest.get("name")?.as_str()?;
            let text_ref = manifest.get("text")?;
            let text = resolve_content_ref(text_ref, blob_base_url, blob_store_path, None).await?;
            Some(Output::stream(name, &text))
        }
        "display_data" | "execute_result" => {
            let data_map = manifest.get("data")?.as_object()?;
            let mut output_data = HashMap::new();

            for (mime_type, content_ref) in data_map {
                if let Some(content) = resolve_content_ref(
                    content_ref,
                    blob_base_url,
                    blob_store_path,
                    Some(mime_type.as_str()),
                )
                .await
                {
                    output_data.insert(mime_type.clone(), content);
                }
            }

            if output_type == "execute_result" {
                let execution_count = manifest.get("execution_count")?.as_i64()?;
                Some(Output::execute_result(output_data, execution_count))
            } else {
                Some(Output::display_data(output_data))
            }
        }
        "error" => {
            let ename = manifest.get("ename")?.as_str()?.to_string();
            let evalue = manifest.get("evalue")?.as_str()?.to_string();

            // Traceback can be a content ref ({"inline": "[...]"}) or a direct array
            let traceback_val = manifest.get("traceback")?;
            let traceback = if let Some(arr) = traceback_val.as_array() {
                // Direct array
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            } else {
                // Content reference - resolve it and parse as JSON array
                let tb_str =
                    resolve_content_ref(traceback_val, blob_base_url, blob_store_path, None)
                        .await?;
                serde_json::from_str::<Vec<String>>(&tb_str).ok()?
            };

            Some(Output::error(&ename, &evalue, traceback))
        }
        _ => None,
    }
}

/// Resolve a content reference from a blob manifest.
///
/// Content refs can be:
/// - `{"inline": "actual content"}` — content is inline
/// - `{"blob": "hash", "size": N}` — content is in the blob store
///
/// For binary MIME types (images, etc.), the blob store holds raw bytes
/// (decoded from Jupyter's base64 wire format). This function reads the
/// raw bytes and base64-encodes them so the Output struct always contains
/// base64 strings for binary content (matching Jupyter conventions).
///
/// For text MIME types, the blob store holds UTF-8 text which is returned
/// as-is.
pub async fn resolve_content_ref(
    content_ref: &serde_json::Value,
    blob_base_url: &Option<String>,
    blob_store_path: &Option<PathBuf>,
    mime_type: Option<&str>,
) -> Option<String> {
    if let Some(inline) = content_ref.get("inline") {
        return inline.as_str().map(|s| s.to_string());
    }

    let blob_hash = content_ref.get("blob").and_then(|v| v.as_str())?;
    let binary = mime_type.is_some_and(is_binary_mime);

    // First try: read directly from disk
    if let Some(store_path) = blob_store_path {
        if blob_hash.len() >= 2 {
            let prefix = &blob_hash[..2];
            let rest = &blob_hash[2..];
            let blob_path = store_path.join(prefix).join(rest);

            if binary {
                // Binary: read raw bytes and base64-encode
                if let Ok(bytes) = tokio::fs::read(&blob_path).await {
                    return Some(base64::engine::general_purpose::STANDARD.encode(&bytes));
                }
            } else {
                // Text: read as UTF-8 string
                if let Ok(contents) = tokio::fs::read_to_string(&blob_path).await {
                    return Some(contents);
                }
            }
        }
    }

    // Second try: fetch from blob server
    if let Some(base_url) = blob_base_url {
        let url = format!("{}/blob/{}", base_url, blob_hash);

        if let Ok(response) = reqwest::get(&url).await {
            if response.status().is_success() {
                if binary {
                    // Binary: read raw bytes and base64-encode
                    if let Ok(bytes) = response.bytes().await {
                        return Some(base64::engine::general_purpose::STANDARD.encode(&bytes));
                    }
                } else {
                    // Text: read as UTF-8 string
                    return response.text().await.ok();
                }
            }
        }
    }

    None
}

/// Resolve all outputs for a cell snapshot.
///
/// Takes the raw output strings from CellSnapshot and resolves them to Output objects.
pub async fn resolve_cell_outputs(
    raw_outputs: &[String],
    blob_base_url: &Option<String>,
    blob_store_path: &Option<PathBuf>,
) -> Vec<Output> {
    let mut outputs = Vec::with_capacity(raw_outputs.len());
    for output_str in raw_outputs {
        if let Some(output) =
            resolve_output_string(output_str, blob_base_url, blob_store_path).await
        {
            outputs.push(output);
        }
    }
    outputs
}
