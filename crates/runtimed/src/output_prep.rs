// Mutex::lock only fails if another thread panicked while holding the lock.
// In that case the program is already crashing, so unwrap is acceptable here.
#![allow(clippy::unwrap_used)]

//! Kernel management for runtime agent subprocesses.
//!
//! Each agent subprocess owns one kernel. The agent manages the kernel lifecycle
//! and execution queue, writing outputs to RuntimeStateDoc which syncs to all
//! connected peers via Automerge.

use anyhow::Result;
use serde::Serialize;

use crate::blob_store::BlobStore;
use crate::output_store::{self, OutputManifest, DEFAULT_INLINE_THRESHOLD};
use notebook_doc::runtime_state::RuntimeStateDoc;

/// Store widget buffers in the blob store and replace values in the state
/// dict with ContentRef objects at the given buffer_paths.
///
/// Uses `{"blob": hash, "size": N, "media_type": "application/octet-stream"}`
/// — the same ContentRef shape as output data and `blob_store_large_state_values`.
///
/// Returns the modified state and the buffer_paths used. If there are no
/// buffers or no buffer_paths, returns the state unchanged and empty paths.
pub(crate) async fn store_widget_buffers(
    state: &serde_json::Value,
    buffer_paths: &[Vec<String>],
    buffers: &[Vec<u8>],
    blob_store: &crate::blob_store::BlobStore,
) -> (serde_json::Value, Vec<Vec<String>>) {
    if buffers.is_empty() || buffer_paths.is_empty() {
        return (state.clone(), vec![]);
    }

    let mut modified = state.clone();
    let mut used_paths = Vec::new();

    for (i, path) in buffer_paths.iter().enumerate() {
        if i >= buffers.len() || path.is_empty() {
            continue;
        }

        // Store buffer in blob store
        let hash = match blob_store
            .put(&buffers[i], "application/octet-stream")
            .await
        {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("[kernel-manager] Failed to store widget buffer: {}", e);
                continue;
            }
        };

        // Navigate to the parent in the state dict and replace with ContentRef.
        // Handles both object keys (strings) and array indices (integers).
        if let Some(last_key) = path.last() {
            let parent_path = &path[..path.len() - 1];
            let parent = parent_path
                .iter()
                .try_fold(&mut modified, |v, key| json_get_mut(v, key));
            if let Some(parent) = parent {
                let content_ref = serde_json::json!({
                    "blob": hash,
                    "size": buffers[i].len(),
                    "media_type": "application/octet-stream",
                });
                if let Some(obj) = parent.as_object_mut() {
                    obj.insert(last_key.clone(), content_ref);
                    used_paths.push(path.clone());
                } else if let Some(arr) = parent.as_array_mut() {
                    if let Ok(idx) = last_key.parse::<usize>() {
                        if idx < arr.len() {
                            arr[idx] = content_ref;
                            used_paths.push(path.clone());
                        }
                    }
                }
            }
        }
    }

    (modified, used_paths)
}

/// Navigate into a JSON value by key (object) or index (array).
fn json_get_mut<'a>(v: &'a mut serde_json::Value, key: &str) -> Option<&'a mut serde_json::Value> {
    match v {
        serde_json::Value::Object(map) => map.get_mut(key),
        serde_json::Value::Array(arr) => key.parse::<usize>().ok().and_then(|idx| arr.get_mut(idx)),
        _ => None,
    }
}

/// Extract buffer_paths from a Jupyter comm data payload.
pub(crate) fn extract_buffer_paths(data: &serde_json::Value) -> Vec<Vec<String>> {
    data.get("buffer_paths")
        .or_else(|| data.get("state").and_then(|s| s.get("buffer_paths")))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|path| {
                    path.as_array().map(|p| {
                        p.iter()
                            .filter_map(|s| {
                                // Handle both string and integer path segments
                                // (ipywidgets uses integers for list indices)
                                s.as_str()
                                    .map(|v| v.to_string())
                                    .or_else(|| s.as_u64().map(|v| v.to_string()))
                                    .or_else(|| s.as_i64().map(|v| v.to_string()))
                            })
                            .collect()
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Threshold (bytes) for blob-storing comm state values.
/// Properties whose JSON serialization exceeds this size are replaced with
/// `{"$blob": "<hash>"}` sentinels. This prevents catastrophically slow
/// Automerge writes for large state (e.g., anywidget `_esm` JS bundles,
/// Vega-Lite `spec` dicts with embedded datasets).
const COMM_STATE_BLOB_THRESHOLD: usize = 1024;

/// Scan the top-level properties of a comm state object and replace any
/// whose JSON-serialized size exceeds `COMM_STATE_BLOB_THRESHOLD` with
/// ContentRef-shaped objects stored in the blob store.
///
/// Uses the same `{"blob": hash, "size": N, "media_type": "..."}` format
/// as output ContentRefs. The optional `media_type` field tells the WASM
/// resolver how to classify the blob (Url for JS/CSS/binary, Blob for
/// JSON/text that needs fetch + parse).
///
/// Only top-level keys are checked — we don't recurse into nested objects.
/// This is intentional: the Automerge cost comes from expanding large nested
/// structures into per-key CRDT entries, and the top-level is where `spec`,
/// `_esm`, and `_css` live.
///
/// Returns the modified state with large values replaced by ContentRefs.
pub(crate) async fn blob_store_large_state_values(
    state: &serde_json::Value,
    blob_store: &BlobStore,
) -> serde_json::Value {
    let Some(obj) = state.as_object() else {
        return state.clone();
    };

    let mut modified = serde_json::Map::with_capacity(obj.len());

    for (key, value) in obj {
        // Serialize to check size. For strings we can check len() directly;
        // for objects/arrays we need the JSON representation.
        let size = match value {
            serde_json::Value::String(s) => s.len(),
            serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                serde_json::to_string(value).map(|s| s.len()).unwrap_or(0)
            }
            // Scalars (bool, number, null) are always small.
            _ => 0,
        };

        if size > COMM_STATE_BLOB_THRESHOLD {
            // Serialize and store in blob store.
            // Use the raw string bytes for strings (preserves content type for
            // _esm JavaScript, _css stylesheets, etc.) and JSON for structured values.
            let (blob_bytes, media_type) = match value {
                serde_json::Value::String(s) => {
                    let mime = match key.as_str() {
                        "_esm" => "text/javascript",
                        "_css" => "text/css",
                        _ => "text/plain",
                    };
                    (s.as_bytes().to_vec(), mime)
                }
                _ => match serde_json::to_vec(value) {
                    Ok(b) => (b, "application/json"),
                    Err(e) => {
                        tracing::warn!(
                            "[kernel-manager] Failed to serialize comm state key '{}': {}",
                            key,
                            e
                        );
                        modified.insert(key.clone(), value.clone());
                        continue;
                    }
                },
            };
            let blob_size = blob_bytes.len();
            match blob_store.put(&blob_bytes, media_type).await {
                Ok(hash) => {
                    modified.insert(
                        key.clone(),
                        serde_json::json!({
                            "blob": hash,
                            "size": blob_size,
                            "media_type": media_type,
                        }),
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "[kernel-manager] Failed to blob-store comm state key '{}' ({} bytes): {}",
                        key,
                        size,
                        e
                    );
                    modified.insert(key.clone(), value.clone());
                }
            }
        } else {
            modified.insert(key.clone(), value.clone());
        }
    }

    serde_json::Value::Object(modified)
}

// ── Launched Environment Config ─────────────────────────────────────────────

/// Re-export environment config types from the shared protocol crate.
/// The canonical definitions live in `notebook_protocol::protocol`.
pub use notebook_protocol::protocol::{DenoLaunchedConfig, LaunchedEnvConfig};

// ── Output Conversion ───────────────────────────────────────────────────────

/// Convert a JupyterMessageContent to nbformat-style JSON for storage in Automerge.
///
/// jupyter_protocol serializes as: `{"ExecuteResult": {"data": {...}, ...}}`
/// nbformat expects: `{"output_type": "execute_result", "data": {...}, ...}`
pub(crate) fn message_content_to_nbformat(
    content: &jupyter_protocol::JupyterMessageContent,
) -> Option<serde_json::Value> {
    use serde_json::json;

    match content {
        jupyter_protocol::JupyterMessageContent::StreamContent(stream) => {
            let name = match stream.name {
                jupyter_protocol::Stdio::Stdout => "stdout",
                jupyter_protocol::Stdio::Stderr => "stderr",
            };
            Some(json!({
                "output_type": "stream",
                "name": name,
                "text": stream.text
            }))
        }
        jupyter_protocol::JupyterMessageContent::DisplayData(data) => {
            let mut output = json!({
                "output_type": "display_data",
                "data": data.data,
                "metadata": data.metadata
            });
            // Preserve display_id for update_display_data targeting
            if let Some(ref transient) = data.transient {
                if let Some(ref display_id) = transient.display_id {
                    output["transient"] = json!({ "display_id": display_id });
                }
            }
            Some(output)
        }
        jupyter_protocol::JupyterMessageContent::ExecuteResult(result) => Some(json!({
            "output_type": "execute_result",
            "data": result.data,
            "metadata": result.metadata,
            "execution_count": result.execution_count.0
        })),
        jupyter_protocol::JupyterMessageContent::ErrorOutput(error) => Some(json!({
            "output_type": "error",
            "ename": error.ename,
            "evalue": error.evalue,
            "traceback": error.traceback
        })),
        _ => None,
    }
}

/// Convert a Jupyter Media bundle (from page payload) to nbformat display_data JSON.
///
/// Page payloads are used by IPython for `?` and `??` help. This converts
/// them to display_data outputs so help content appears in cell outputs.
pub(crate) fn media_to_display_data(media: &jupyter_protocol::Media) -> serde_json::Value {
    serde_json::json!({
        "output_type": "display_data",
        "data": media,
        "metadata": {}
    })
}

/// Update an output by display_id when outputs are inline manifests.
///
/// Updates all outputs matching a display_id with new data and metadata.
///
/// Uses the `display_index` for O(1) lookup of matching outputs. Falls back
/// to a full scan if the index has no entries (legacy outputs before indexing).
///
/// Returns true if at least one output was updated, false otherwise.
pub(crate) async fn update_output_by_display_id_with_manifests(
    state_doc: &mut RuntimeStateDoc,
    display_id: &str,
    new_data: &serde_json::Value,
    new_metadata: &serde_json::Map<String, serde_json::Value>,
    blob_store: &BlobStore,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let index_entries = state_doc.get_display_index_entries(display_id);

    if !index_entries.is_empty() {
        let mut found = false;
        for (exec_id, target_output_id) in &index_entries {
            let outputs = state_doc.get_outputs(exec_id);
            for (output_idx, output_value) in outputs.into_iter().enumerate() {
                let manifest: OutputManifest = match serde_json::from_value(output_value) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if manifest.output_id() != target_output_id {
                    continue;
                }
                if let Some(updated) = output_store::update_manifest_display_data(
                    &manifest,
                    display_id,
                    new_data,
                    new_metadata,
                    blob_store,
                    DEFAULT_INLINE_THRESHOLD,
                )
                .await?
                {
                    state_doc.replace_output(exec_id, output_idx, &updated.to_json())?;
                    found = true;
                }
            }
        }
        Ok(found)
    } else {
        // Fallback: full scan for outputs without display_index entries
        let outputs = state_doc.get_all_outputs();
        let mut found = false;
        for (exec_id, output_idx, output_value) in outputs {
            let manifest: OutputManifest = match serde_json::from_value(output_value) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if let Some(updated) = output_store::update_manifest_display_data(
                &manifest,
                display_id,
                new_data,
                new_metadata,
                blob_store,
                DEFAULT_INLINE_THRESHOLD,
            )
            .await?
            {
                state_doc.replace_output(&exec_id, output_idx, &updated.to_json())?;
                found = true;
            }
        }
        Ok(found)
    }
}

/// A cell queued for execution.
#[derive(Debug, Clone)]
pub struct QueuedCell {
    pub cell_id: String,
    pub execution_id: String,
    pub code: String,
}

/// Kernel status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelStatus {
    /// Kernel is starting up
    Starting,
    /// Kernel is ready and idle
    Idle,
    /// Kernel is executing code
    Busy,
    /// Kernel encountered an error
    Error,
    /// Kernel is shutting down
    ShuttingDown,
    /// Kernel process died unexpectedly
    Dead,
}

impl std::fmt::Display for KernelStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KernelStatus::Starting => write!(f, "starting"),
            KernelStatus::Idle => write!(f, "idle"),
            KernelStatus::Busy => write!(f, "busy"),
            KernelStatus::Error => write!(f, "error"),
            KernelStatus::ShuttingDown => write!(f, "shutdown"),
            // Dead maps to "error" for frontend compatibility (frontend only recognizes
            // not_started, starting, idle, busy, error, shutdown)
            KernelStatus::Dead => write!(f, "error"),
        }
    }
}

/// Commands from iopub/shell handlers for queue state management.
///
/// These are sent from spawned tasks and must be processed by code
/// that has access to the kernel (e.g., the runtime agent).
#[derive(Debug)]
pub enum QueueCommand {
    /// A cell finished executing (received status=idle from kernel)
    ExecutionDone {
        cell_id: String,
        execution_id: String,
    },
    /// A cell produced an error (for stop-on-error behavior)
    CellError {
        cell_id: String,
        execution_id: String,
    },
    /// The kernel process died (iopub connection lost).
    /// Unblocks the execution queue and notifies the frontend.
    KernelDied,
    /// Send a comm_msg(update) to the kernel via the shell channel.
    /// Used by the IOPub task to sync Output widget captured outputs back.
    SendCommUpdate {
        comm_id: String,
        state: serde_json::Value,
    },
}

/// Escape a search pattern for IPython's fnmatch-based history search.
///
/// IPython's history search uses fnmatch (glob) matching, so we need to:
/// 1. Escape any glob metacharacters in the user's search term
/// 2. Wrap with *...* for substring matching
///
/// Without this, a search for "for" would only match entries exactly equal
/// to "for", not entries containing "for".
pub(crate) fn escape_glob_pattern(pattern: Option<&str>) -> String {
    match pattern {
        Some(p) if !p.is_empty() => {
            let mut escaped = String::with_capacity(p.len() + 2);
            escaped.push('*');
            for c in p.chars() {
                match c {
                    '*' | '?' | '[' | ']' => {
                        escaped.push('[');
                        escaped.push(c);
                        escaped.push(']');
                    }
                    _ => escaped.push(c),
                }
            }
            escaped.push('*');
            escaped
        }
        _ => "*".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kernel_status_display() {
        assert_eq!(KernelStatus::Starting.to_string(), "starting");
        assert_eq!(KernelStatus::Idle.to_string(), "idle");
        assert_eq!(KernelStatus::Busy.to_string(), "busy");
        assert_eq!(KernelStatus::Error.to_string(), "error");
        assert_eq!(KernelStatus::ShuttingDown.to_string(), "shutdown");
    }

    #[test]
    fn test_kernel_status_serialize() {
        let json = serde_json::to_string(&KernelStatus::Idle).unwrap();
        assert_eq!(json, "\"idle\"");
    }

    #[test]
    fn test_escape_glob_pattern_none() {
        assert_eq!(escape_glob_pattern(None), "*");
    }

    #[test]
    fn test_escape_glob_pattern_empty() {
        assert_eq!(escape_glob_pattern(Some("")), "*");
    }

    #[test]
    fn test_escape_glob_pattern_simple() {
        assert_eq!(escape_glob_pattern(Some("for")), "*for*");
        assert_eq!(escape_glob_pattern(Some("import time")), "*import time*");
    }

    #[test]
    fn test_escape_glob_pattern_metacharacters() {
        // Each glob metacharacter should be wrapped in brackets to escape it
        assert_eq!(escape_glob_pattern(Some("*")), "*[*]*");
        assert_eq!(escape_glob_pattern(Some("?")), "*[?]*");
        assert_eq!(escape_glob_pattern(Some("[test]")), "*[[]test[]]*");
    }

    #[test]
    fn test_escape_glob_pattern_mixed() {
        // Complex pattern with multiple metacharacters
        assert_eq!(escape_glob_pattern(Some("a*b?c[d]")), "*a[*]b[?]c[[]d[]]*");
    }

    // ── update_output_by_display_id_with_manifests tests ──────────────

    use tempfile::TempDir;

    fn test_blob_store(dir: &TempDir) -> BlobStore {
        BlobStore::new(dir.path().join("blobs"))
    }

    /// Helper: create a display_data manifest with a display_id and append
    /// the inline manifest to the given execution in the state doc.
    async fn insert_display_output(
        state_doc: &mut RuntimeStateDoc,
        execution_id: &str,
        display_id: &str,
        text_content: &str,
        blob_store: &BlobStore,
    ) -> serde_json::Value {
        let nbformat = serde_json::json!({
            "output_type": "display_data",
            "data": { "text/plain": text_content },
            "metadata": {},
            "transient": { "display_id": display_id }
        });
        let manifest =
            output_store::create_manifest(&nbformat, blob_store, DEFAULT_INLINE_THRESHOLD)
                .await
                .unwrap();
        let manifest_json = manifest.to_json();
        state_doc
            .append_output(execution_id, &manifest_json)
            .unwrap();
        manifest_json
    }

    /// Extract the text/plain inline content from an output manifest Value.
    fn read_text_plain(manifest: &serde_json::Value) -> String {
        manifest["data"]["text/plain"]["inline"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn test_update_display_id_updates_all_matching_outputs() {
        let dir = TempDir::new().unwrap();
        let store = test_blob_store(&dir);
        let mut state_doc = RuntimeStateDoc::new();

        // Two executions, both with the same display_id (simulates re-running a cell)
        state_doc.create_execution("exec-1", "cell-a");
        state_doc.create_execution("exec-2", "cell-a");
        insert_display_output(&mut state_doc, "exec-1", "progress", "old", &store).await;
        insert_display_output(&mut state_doc, "exec-2", "progress", "old", &store).await;

        let new_data = serde_json::json!({ "text/plain": "updated" });
        let new_metadata = serde_json::Map::new();
        let result = update_output_by_display_id_with_manifests(
            &mut state_doc,
            "progress",
            &new_data,
            &new_metadata,
            &store,
        )
        .await
        .unwrap();

        assert!(result, "should report that outputs were updated");

        // Both executions' outputs should now contain "updated"
        let outputs_1 = state_doc.get_outputs("exec-1");
        let outputs_2 = state_doc.get_outputs("exec-2");
        assert_eq!(outputs_1.len(), 1);
        assert_eq!(outputs_2.len(), 1);
        assert_eq!(read_text_plain(&outputs_1[0]), "updated");
        assert_eq!(read_text_plain(&outputs_2[0]), "updated");
    }

    #[tokio::test]
    async fn test_update_display_id_no_match_returns_false() {
        let dir = TempDir::new().unwrap();
        let store = test_blob_store(&dir);
        let mut state_doc = RuntimeStateDoc::new();

        state_doc.create_execution("exec-1", "cell-a");
        insert_display_output(&mut state_doc, "exec-1", "progress", "hello", &store).await;

        let new_data = serde_json::json!({ "text/plain": "updated" });
        let new_metadata = serde_json::Map::new();
        let result = update_output_by_display_id_with_manifests(
            &mut state_doc,
            "nonexistent-id",
            &new_data,
            &new_metadata,
            &store,
        )
        .await
        .unwrap();

        assert!(!result, "should return false when no display_id matches");

        // Original output unchanged
        let outputs = state_doc.get_outputs("exec-1");
        assert_eq!(read_text_plain(&outputs[0]), "hello");
    }

    #[tokio::test]
    async fn test_update_display_id_only_updates_matching() {
        let dir = TempDir::new().unwrap();
        let store = test_blob_store(&dir);
        let mut state_doc = RuntimeStateDoc::new();

        state_doc.create_execution("exec-1", "cell-a");
        state_doc.create_execution("exec-2", "cell-b");
        insert_display_output(&mut state_doc, "exec-1", "progress", "match-me", &store).await;
        insert_display_output(&mut state_doc, "exec-2", "other-id", "leave-me", &store).await;

        let new_data = serde_json::json!({ "text/plain": "updated" });
        let new_metadata = serde_json::Map::new();
        let result = update_output_by_display_id_with_manifests(
            &mut state_doc,
            "progress",
            &new_data,
            &new_metadata,
            &store,
        )
        .await
        .unwrap();

        assert!(result);

        // Only the matching output should be updated
        let outputs_1 = state_doc.get_outputs("exec-1");
        let outputs_2 = state_doc.get_outputs("exec-2");
        assert_eq!(read_text_plain(&outputs_1[0]), "updated");
        assert_eq!(read_text_plain(&outputs_2[0]), "leave-me");
    }
}
