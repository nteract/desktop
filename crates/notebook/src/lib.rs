pub mod cli_install;
pub mod conda_env;
pub mod deno_env;
pub mod env_pool;
pub mod environment_yml;
pub mod format;
pub mod menu;
pub mod notebook_state;
pub mod pixi;
pub mod project_file;
pub mod pyproject;
pub mod runtime;
pub mod settings;
pub mod shell_env;
// Re-export tools from kernel-launch crate
pub use kernel_launch::tools;
pub mod trust;
pub mod typosquat;
pub mod uv_env;
#[cfg(feature = "webdriver-test")]
pub mod webdriver;

pub use runtime::Runtime;

use notebook_state::{FrontendCell, NotebookState};
use runtimed::notebook_doc::CellSnapshot;
use runtimed::notebook_sync_client::{NotebookSyncClient, NotebookSyncHandle};
use runtimed::protocol::{CompletionItem, HistoryEntry, NotebookRequest, NotebookResponse};

use log::{debug, info, warn};
use nbformat::v4::{Cell, CellId, CellMetadata};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::OsStr;

/// Shared notebook sync handle for cross-window state synchronization.
/// The Option allows graceful fallback when daemon is unavailable.
/// Uses the split handle pattern - the handle is clonable and doesn't block.
type SharedNotebookSync = Arc<tokio::sync::Mutex<Option<NotebookSyncHandle>>>;

#[derive(Clone)]
struct WindowNotebookContext {
    notebook_state: Arc<Mutex<NotebookState>>,
    notebook_sync: SharedNotebookSync,
}

#[derive(Clone, Default)]
struct WindowNotebookRegistry {
    contexts: Arc<Mutex<HashMap<String, WindowNotebookContext>>>,
}

impl WindowNotebookRegistry {
    fn insert(
        &self,
        label: impl Into<String>,
        context: WindowNotebookContext,
    ) -> Result<(), String> {
        let mut contexts = self.contexts.lock().map_err(|e| e.to_string())?;
        contexts.insert(label.into(), context);
        Ok(())
    }

    fn get(&self, label: &str) -> Result<WindowNotebookContext, String> {
        let contexts = self.contexts.lock().map_err(|e| e.to_string())?;
        contexts
            .get(label)
            .cloned()
            .ok_or_else(|| format!("No notebook context for window '{label}'"))
    }
}

/// Newtype wrapper for auto-launch-in-progress flag (distinguishes from other AtomicBool states).
#[allow(dead_code)]
struct AutoLaunchInProgress(Arc<AtomicBool>);

/// Newtype wrapper for reconnect-in-progress flag (distinguishes from other AtomicBool states).
struct ReconnectInProgress(Arc<AtomicBool>);

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(any(target_os = "macos", target_os = "ios"))]
use tauri::RunEvent;
use tauri::{Emitter, Manager};

#[derive(Serialize)]
struct KernelspecInfo {
    name: String,
    display_name: String,
    language: String,
}

/// Git information for debug banner display.
#[derive(Serialize)]
struct GitInfo {
    branch: String,
    commit: String,
    description: Option<String>,
}

/// Environment sync state for dirty detection.
#[derive(Serialize)]
#[serde(tag = "status")]
pub enum EnvSyncState {
    /// Kernel is not running
    #[serde(rename = "not_running")]
    NotRunning,
    /// Kernel is running but not UV-managed
    #[serde(rename = "not_uv_managed")]
    NotUvManaged,
    /// Environment is in sync with declared dependencies
    #[serde(rename = "synced")]
    Synced,
    /// Environment differs from declared dependencies
    #[serde(rename = "dirty")]
    Dirty {
        /// Dependencies declared but not synced
        added: Vec<String>,
        /// Dependencies synced but no longer declared
        removed: Vec<String>,
    },
}

/// Derive a notebook ID for sync purposes.
///
/// For saved notebooks, uses the canonical file path (stable across processes).
/// For unsaved notebooks, uses the env_id from metadata (random UUID).
fn derive_notebook_id(state: &NotebookState) -> String {
    match &state.path {
        Some(path) => {
            // Use canonical path for deterministic ID across processes
            path.canonicalize()
                .unwrap_or_else(|_| path.clone())
                .to_string_lossy()
                .to_string()
        }
        None => {
            // Unsaved notebook - use env_id from metadata (already generated)
            state
                .notebook
                .metadata
                .additional
                .get("runt")
                .and_then(|v| v.get("env_id"))
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
        }
    }
}

fn notebook_state_for_window(
    window: &tauri::Window,
    registry: &WindowNotebookRegistry,
) -> Result<Arc<Mutex<NotebookState>>, String> {
    Ok(registry.get(window.label())?.notebook_state)
}

fn notebook_sync_for_window(
    window: &tauri::Window,
    registry: &WindowNotebookRegistry,
) -> Result<SharedNotebookSync, String> {
    Ok(registry.get(window.label())?.notebook_sync)
}

fn emit_to_label<R, M, S>(emitter: &M, label: &str, event: &str, payload: S) -> tauri::Result<()>
where
    R: tauri::Runtime,
    M: tauri::Emitter<R>,
    S: Serialize + Clone,
{
    emitter.emit_to(
        tauri::EventTarget::webview(label.to_string()),
        event,
        payload,
    )
}

/// Convert a CellSnapshot from Automerge to an nbformat Cell.
/// Used when joining an existing room to update local state.
fn cell_snapshot_to_nbformat(snap: &CellSnapshot) -> Cell {
    let id = CellId::from(uuid::Uuid::parse_str(&snap.id).unwrap_or_else(|_| uuid::Uuid::new_v4()));
    let source: Vec<String> = if snap.source.is_empty() {
        Vec::new()
    } else {
        snap.source
            .split_inclusive('\n')
            .map(|s| s.to_string())
            .collect()
    };
    let metadata = CellMetadata {
        id: None,
        collapsed: None,
        scrolled: None,
        deletable: None,
        editable: None,
        format: None,
        name: None,
        tags: None,
        jupyter: None,
        execution: None,
        additional: std::collections::HashMap::new(),
    };

    match snap.cell_type.as_str() {
        "code" => {
            let execution_count = if snap.execution_count == "null" {
                None
            } else {
                snap.execution_count.parse().ok()
            };
            // Parse outputs from JSON strings
            let outputs: Vec<nbformat::v4::Output> = snap
                .outputs
                .iter()
                .filter_map(|json_str| serde_json::from_str(json_str).ok())
                .collect();
            Cell::Code {
                id,
                metadata,
                execution_count,
                source,
                outputs,
            }
        }
        "markdown" => Cell::Markdown {
            id,
            metadata,
            source,
            attachments: None,
        },
        _ => Cell::Raw {
            id,
            metadata,
            source,
        },
    }
}

/// Initialize notebook sync with the daemon.
///
/// Connects to the daemon's notebook sync service using the split pattern,
/// populates the Automerge doc if this is a new room, and spawns a background
/// task to receive changes from other peers (cross-window sync).
///
/// The split pattern separates the handle (for sending commands) from the
/// receiver (for incoming changes), avoiding lock contention during network I/O.
async fn initialize_notebook_sync(
    window: tauri::WebviewWindow,
    notebook_state: Arc<Mutex<NotebookState>>,
    notebook_sync: SharedNotebookSync,
) -> Result<(), String> {
    let (notebook_id, cells) = {
        let state = notebook_state.lock().map_err(|e| e.to_string())?;
        (derive_notebook_id(&state), state.cells_for_frontend())
    };

    let socket_path = runtimed::default_socket_path();
    info!(
        "[notebook-sync] Connecting to daemon for notebook: {} ({})",
        notebook_id,
        socket_path.display()
    );

    // Connect using the split pattern - returns handle, receiver, broadcast receiver, initial cells, and initial metadata
    let (handle, mut receiver, mut broadcast_receiver, initial_cells, initial_metadata) =
        NotebookSyncClient::connect_split(socket_path, notebook_id.clone())
            .await
            .map_err(|e| format!("sync connect: {}", e))?;

    // Populate Automerge doc if empty (new room or first window)
    if initial_cells.is_empty() {
        info!(
            "[notebook-sync] Populating Automerge doc with {} cells",
            cells.len()
        );
        for (i, cell) in cells.iter().enumerate() {
            let (id, cell_type, source) = match cell {
                FrontendCell::Code { id, source, .. } => (id.as_str(), "code", source.as_str()),
                FrontendCell::Markdown { id, source } => (id.as_str(), "markdown", source.as_str()),
                FrontendCell::Raw { id, source } => (id.as_str(), "raw", source.as_str()),
            };
            handle
                .add_cell(i, id, cell_type)
                .await
                .map_err(|e| format!("add_cell: {}", e))?;
            if !source.is_empty() {
                handle
                    .update_source(id, source)
                    .await
                    .map_err(|e| format!("update_source: {}", e))?;
            }
        }

        // Also push notebook metadata to Automerge doc
        let metadata_json = {
            let state = notebook_state.lock().map_err(|e| e.to_string())?;
            let snapshot = notebook_state::snapshot_from_nbformat(&state.notebook.metadata);
            serde_json::to_string(&snapshot).map_err(|e| format!("serialize metadata: {}", e))?
        };
        info!(
            "[notebook-sync] Pushing metadata to Automerge doc for {}",
            notebook_id
        );
        handle
            .set_metadata(
                runtimed::notebook_metadata::NOTEBOOK_METADATA_KEY,
                &metadata_json,
            )
            .await
            .map_err(|e| format!("set_metadata: {}", e))?;
    } else {
        info!(
            "[notebook-sync] Joining existing room with {} cells",
            initial_cells.len()
        );
        // Update local NotebookState to match Automerge state
        // This prevents race conditions where load_notebook returns stale disk content
        {
            let mut state = notebook_state.lock().map_err(|e| e.to_string())?;
            state.notebook.cells = initial_cells
                .iter()
                .map(cell_snapshot_to_nbformat)
                .collect();
            info!(
                "[notebook-sync] Updated local state with {} cells from Automerge",
                state.notebook.cells.len()
            );

            // Also merge metadata from Automerge doc into local state
            if let Some(ref metadata_json) = initial_metadata {
                match serde_json::from_str::<runtimed::notebook_metadata::NotebookMetadataSnapshot>(
                    metadata_json,
                ) {
                    Ok(snapshot) => {
                        notebook_state::merge_snapshot_into_nbformat(
                            &snapshot,
                            &mut state.notebook.metadata,
                        );
                        info!(
                            "[notebook-sync] Merged metadata from Automerge for {}",
                            notebook_id
                        );
                    }
                    Err(e) => {
                        warn!(
                            "[notebook-sync] Failed to deserialize metadata from Automerge: {}",
                            e
                        );
                    }
                }
            }
        }
        // Emit Automerge state to frontend (for immediate UI update)
        if let Err(e) =
            emit_to_label::<_, _, _>(&window, window.label(), "notebook:updated", &initial_cells)
        {
            warn!("[notebook-sync] Failed to emit initial cells: {}", e);
        }
    }

    // Store the handle for commands to use
    info!(
        "[notebook-sync] Storing handle for {} (prior state: {:?})",
        notebook_id,
        notebook_sync.lock().await.is_some()
    );
    *notebook_sync.lock().await = Some(handle);
    info!(
        "[notebook-sync] Handle stored successfully for {}",
        notebook_id
    );

    // Spawn receiver task for cross-window sync
    // The receiver is separate from the handle, so it doesn't block commands
    let window_clone = window.clone();
    let notebook_id_for_receiver = notebook_id.clone();
    let notebook_state_for_receiver = notebook_state.clone();
    tokio::spawn(async move {
        info!(
            "[notebook-sync] Starting receiver loop for {}",
            notebook_id_for_receiver
        );
        while let Some(update) = receiver.recv().await {
            info!(
                "[notebook-sync] Received {} cells from peer for {}",
                update.cells.len(),
                notebook_id_for_receiver
            );
            // Emit cell changes for frontend to reconcile state
            if let Err(e) = emit_to_label::<_, _, _>(
                &window_clone,
                window_clone.label(),
                "notebook:updated",
                &update.cells,
            ) {
                warn!("[notebook-sync] Failed to emit notebook:updated: {}", e);
            }

            // If metadata changed, merge into local state and notify frontend
            if let Some(ref metadata_json) = update.notebook_metadata {
                match serde_json::from_str::<runtimed::notebook_metadata::NotebookMetadataSnapshot>(
                    metadata_json,
                ) {
                    Ok(snapshot) => {
                        if let Ok(mut state) = notebook_state_for_receiver.lock() {
                            notebook_state::merge_snapshot_into_nbformat(
                                &snapshot,
                                &mut state.notebook.metadata,
                            );
                        }
                        if let Err(e) = emit_to_label::<_, _, _>(
                            &window_clone,
                            window_clone.label(),
                            "notebook:metadata_updated",
                            metadata_json,
                        ) {
                            warn!(
                                "[notebook-sync] Failed to emit notebook:metadata_updated: {}",
                                e
                            );
                        }
                    }
                    Err(e) => {
                        warn!(
                            "[notebook-sync] Failed to deserialize metadata from peer: {}",
                            e
                        );
                    }
                }
            }
        }
        info!(
            "[notebook-sync] Receiver loop ended for {} - changes_tx was dropped",
            notebook_id_for_receiver
        );
    });

    // Clone app for later use (before spawning moves it)
    let window_for_ready = window.clone();

    // Spawn broadcast receiver task for daemon kernel events
    let notebook_sync_for_disconnect = notebook_sync.clone();
    let notebook_id_for_broadcast = notebook_id.clone();
    tokio::spawn(async move {
        info!(
            "[notebook-sync] Starting broadcast receiver loop for {}",
            notebook_id_for_broadcast
        );
        while let Some(broadcast) = broadcast_receiver.recv().await {
            info!(
                "[notebook-sync] Received broadcast for {}: {:?}",
                notebook_id_for_broadcast, broadcast
            );
            // Emit broadcast events to frontend
            if let Err(e) =
                emit_to_label::<_, _, _>(&window, window.label(), "daemon:broadcast", &broadcast)
            {
                warn!("[notebook-sync] Failed to emit daemon:broadcast: {}", e);
            }
        }
        warn!(
            "[notebook-sync] Broadcast receiver loop ended for {} - daemon disconnected (broadcast_tx dropped)",
            notebook_id_for_broadcast
        );

        // Clear the handle so operations fail gracefully
        info!(
            "[notebook-sync] Clearing notebook_sync handle for {}",
            notebook_id_for_broadcast
        );
        *notebook_sync_for_disconnect.lock().await = None;
        info!(
            "[notebook-sync] Handle cleared for {}",
            notebook_id_for_broadcast
        );

        // Emit disconnection event so frontend can reset kernel state
        if let Err(e) = emit_to_label::<_, _, _>(&window, window.label(), "daemon:disconnected", ())
        {
            warn!("[notebook-sync] Failed to emit daemon:disconnected: {}", e);
        }
    });

    info!(
        "[notebook-sync] Initialization complete for {}",
        notebook_id
    );

    // Emit event so frontend knows daemon sync is ready
    // Frontend should wait for this before calling daemon commands
    if let Err(e) = emit_to_label::<_, _, _>(
        &window_for_ready,
        window_for_ready.label(),
        "daemon:ready",
        (),
    ) {
        warn!("[notebook-sync] Failed to emit daemon:ready: {}", e);
    }

    Ok(())
}

/// Push the current notebook metadata to the Automerge doc via the sync handle.
///
/// Call this after any mutation to `notebook.metadata` so that the daemon
/// and other windows receive the updated metadata through Automerge sync.
async fn push_metadata_to_sync(
    notebook_state: &Arc<Mutex<NotebookState>>,
    notebook_sync: &SharedNotebookSync,
) {
    let metadata_json = {
        let state = match notebook_state.lock() {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    "[notebook-sync] Failed to lock state for metadata push: {}",
                    e
                );
                return;
            }
        };
        let snapshot = notebook_state::snapshot_from_nbformat(&state.notebook.metadata);
        match serde_json::to_string(&snapshot) {
            Ok(json) => json,
            Err(e) => {
                warn!(
                    "[notebook-sync] Failed to serialize metadata snapshot: {}",
                    e
                );
                return;
            }
        }
    };

    let handle = notebook_sync.lock().await.clone();
    if let Some(handle) = handle {
        if let Err(e) = handle
            .set_metadata(
                runtimed::notebook_metadata::NOTEBOOK_METADATA_KEY,
                &metadata_json,
            )
            .await
        {
            warn!(
                "[notebook-sync] Failed to push metadata to Automerge: {}",
                e
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::next_available_sample_path;
    use tempfile::TempDir;

    #[test]
    fn next_available_sample_path_reuses_original_name_when_available() {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = next_available_sample_path(temp_dir.path(), "example.ipynb");
        assert_eq!(path, temp_dir.path().join("example.ipynb"));
    }

    #[test]
    fn next_available_sample_path_adds_suffix_for_collisions() {
        let temp_dir = TempDir::new().expect("temp dir");
        let original = temp_dir.path().join("example.ipynb");
        std::fs::write(&original, "{}").expect("create existing file");

        let path = next_available_sample_path(temp_dir.path(), "example.ipynb");

        assert_eq!(path, temp_dir.path().join("example-2.ipynb"));
    }
}

/// Get the path to the bundled runtimed binary.
///
/// Tauri places external binaries differently depending on the build type:
/// - Bundled macOS apps: Contents/MacOS/runtimed (no target suffix)
/// - Development/no-bundle: target/{debug,release}/binaries/runtimed-{target}
fn get_bundled_runtimed_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    // First, try the bundled app location
    // Tauri places externalBin differently per platform:
    // - macOS: Contents/MacOS/runtimed
    // - Linux: next to executable or in ../lib/{app}/
    // - Windows: next to executable
    #[cfg(target_os = "macos")]
    {
        if let Ok(exe_dir) = app.path().resource_dir() {
            // resource_dir on macOS points to Contents/Resources
            // The binary is in Contents/MacOS, which is ../MacOS from Resources
            let macos_dir = exe_dir.parent()?.join("MacOS");
            let bundled_path = macos_dir.join("runtimed");
            if bundled_path.exists() {
                log::debug!("[startup] Found bundled runtimed at {:?}", bundled_path);
                return Some(bundled_path);
            }
            log::debug!("[startup] Bundled runtimed not found at {:?}", bundled_path);
        }
    }

    #[cfg(target_os = "linux")]
    {
        // On Linux, Tauri places binaries next to the executable
        if let Ok(resource_dir) = app.path().resource_dir() {
            let bundled_path = resource_dir.join("runtimed");
            if bundled_path.exists() {
                log::debug!("[startup] Found bundled runtimed at {:?}", bundled_path);
                return Some(bundled_path);
            }
            log::debug!("[startup] Bundled runtimed not found at {:?}", bundled_path);
        }
    }

    #[cfg(target_os = "windows")]
    {
        // On Windows, Tauri places binaries next to the executable
        if let Ok(resource_dir) = app.path().resource_dir() {
            let bundled_path = resource_dir.join("runtimed.exe");
            if bundled_path.exists() {
                log::debug!("[startup] Found bundled runtimed at {:?}", bundled_path);
                return Some(bundled_path);
            }
            log::debug!("[startup] Bundled runtimed not found at {:?}", bundled_path);
        }
    }

    // Fallback: try the development path (target/*/binaries/runtimed-{target})
    let target = if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "aarch64-apple-darwin"
        } else {
            "x86_64-apple-darwin"
        }
    } else if cfg!(target_os = "linux") {
        if cfg!(target_arch = "aarch64") {
            "aarch64-unknown-linux-gnu"
        } else {
            "x86_64-unknown-linux-gnu"
        }
    } else if cfg!(target_os = "windows") {
        "x86_64-pc-windows-msvc"
    } else {
        return None;
    };

    let binary_name = if cfg!(windows) {
        format!("runtimed-{}.exe", target)
    } else {
        format!("runtimed-{}", target)
    };

    // Try to find it relative to the executable (for no-bundle dev builds)
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            // Check binaries/ directory next to the executable
            let dev_path = exe_dir.join("binaries").join(&binary_name);
            if dev_path.exists() {
                log::debug!("[startup] Found dev runtimed at {:?}", dev_path);
                return Some(dev_path);
            }
            log::debug!("[startup] Dev runtimed not found at {:?}", dev_path);
        }
    }

    None
}

/// Get git information for the debug banner.
/// Returns None in release builds.
#[tauri::command]
async fn get_git_info() -> Option<GitInfo> {
    #[cfg(debug_assertions)]
    {
        // Try to read workspace description from .context/workspace-description
        let description = std::fs::read_to_string(".context/workspace-description")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        Some(GitInfo {
            branch: env!("GIT_BRANCH").to_string(),
            commit: env!("GIT_COMMIT").to_string(),
            description,
        })
    }
    #[cfg(not(debug_assertions))]
    {
        None
    }
}

/// Get the current status of the prewarming UV environment pool.
/// Returns None in release builds.
#[tauri::command]
async fn get_prewarm_status(
    pool: tauri::State<'_, env_pool::SharedEnvPool>,
) -> Result<Option<env_pool::PoolStatus>, String> {
    #[cfg(debug_assertions)]
    {
        let p = pool.lock().await;
        Ok(Some(p.status()))
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = pool; // Silence unused warning
        Ok(None)
    }
}

/// Get the current status of the prewarming conda environment pool.
/// Returns None in release builds.
#[tauri::command]
async fn get_conda_pool_status(
    pool: tauri::State<'_, env_pool::SharedCondaEnvPool>,
) -> Result<Option<env_pool::CondaPoolStatus>, String> {
    #[cfg(debug_assertions)]
    {
        let p = pool.lock().await;
        Ok(Some(p.status()))
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = pool; // Silence unused warning
        Ok(None)
    }
}

/// Daemon info for debug banner display.
#[derive(Clone, serde::Serialize)]
pub struct DaemonInfoForBanner {
    pub version: String,
    pub socket_path: String,
    pub is_dev_mode: bool,
}

/// Get daemon info for the debug banner.
/// Returns None in release builds or if daemon.json doesn't exist.
#[tauri::command]
async fn get_daemon_info() -> Option<DaemonInfoForBanner> {
    #[cfg(debug_assertions)]
    {
        // Use runtimed's path resolution which handles dev mode (per-worktree) paths
        let info_path = runtimed::singleton::daemon_info_path();
        let contents = std::fs::read_to_string(info_path).ok()?;
        let json: serde_json::Value = serde_json::from_str(&contents).ok()?;
        let version = json.get("version")?.as_str()?.to_string();
        // Read the actual endpoint from daemon.json (supports custom --socket)
        let socket_path_full = json
            .get("endpoint")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                runtimed::default_socket_path()
                    .to_string_lossy()
                    .to_string()
            });
        // Replace home directory with ~ for shorter display
        let socket_path = if let Some(home) = dirs::home_dir() {
            let home_str = home.to_string_lossy();
            if socket_path_full.starts_with(home_str.as_ref()) {
                socket_path_full.replacen(home_str.as_ref(), "~", 1)
            } else {
                socket_path_full
            }
        } else {
            socket_path_full
        };
        let is_dev_mode = runtimed::is_dev_mode();
        Some(DaemonInfoForBanner {
            version,
            socket_path,
            is_dev_mode,
        })
    }
    #[cfg(not(debug_assertions))]
    {
        None
    }
}

/// Get the blob server port from the running daemon.
/// Used by the frontend to resolve manifest hashes to outputs.
#[tauri::command]
async fn get_blob_port() -> Result<u16, String> {
    let info = runtimed::singleton::get_running_daemon_info()
        .ok_or_else(|| "Daemon not running".to_string())?;
    info.blob_port
        .ok_or_else(|| "Blob server not available".to_string())
}

#[tauri::command]
async fn load_notebook(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<Vec<FrontendCell>, String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let state = state.lock().map_err(|e| e.to_string())?;
    Ok(state.cells_for_frontend())
}

/// Check if the notebook has a file path set
#[tauri::command]
async fn has_notebook_path(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<bool, String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let state = state.lock().map_err(|e| e.to_string())?;
    Ok(state.path.is_some())
}

/// Get the current notebook file path
#[tauri::command]
async fn get_notebook_path(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<Option<String>, String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let state = state.lock().map_err(|e| e.to_string())?;
    Ok(state.path.as_ref().map(|p| p.to_string_lossy().to_string()))
}

/// Format all code cells in the notebook and save.
/// Formatting is best-effort - cells that fail to format are saved as-is.
///
/// Save path: daemon writes .ipynb to disk (merging synced metadata with
/// existing file content). Falls back to local save if daemon is unavailable.
#[tauri::command]
async fn save_notebook(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    // First pass: collect cells to format (release lock for async formatting)
    let (runtime, cells_to_format, path) = {
        let nb = state.lock().map_err(|e| e.to_string())?;
        let path = nb
            .path
            .clone()
            .ok_or_else(|| "No file path set - use save_notebook_as".to_string())?;
        let rt = nb.get_runtime();

        // Collect all code cells with their sources
        let cells: Vec<(String, String)> = nb
            .notebook
            .cells
            .iter()
            .filter_map(|cell| {
                if let nbformat::v4::Cell::Code { id, source, .. } = cell {
                    let src = source.join("");
                    if !src.trim().is_empty() {
                        return Some((id.to_string(), src));
                    }
                }
                None
            })
            .collect();

        (rt, cells, path)
    };

    // Format each cell (async, outside the lock)
    for (cell_id, source) in cells_to_format {
        let format_result = match runtime {
            Runtime::Python => format::format_python(&source).await,
            Runtime::Deno => format::format_deno(&source, "typescript").await,
            Runtime::Other(_) => Err(anyhow::anyhow!("No formatter for unknown runtime")),
        };

        if let Ok(result) = format_result {
            let cell_source = result.source_for_cell();
            if cell_source != source {
                // Update notebook state with formatted code
                {
                    let mut nb = state.lock().map_err(|e| e.to_string())?;
                    nb.update_cell_source(&cell_id, cell_source);
                }
                // Emit event to sync frontend
                let _ = emit_to_label::<_, _, _>(
                    &window,
                    window.label(),
                    "cell:source_updated",
                    serde_json::json!({
                        "cell_id": cell_id,
                        "source": cell_source,
                    }),
                );
            }
        }
        // Formatting errors are silently ignored - save with original code
    }

    // Ensure latest metadata is pushed to daemon before saving
    push_metadata_to_sync(&state, &notebook_sync).await;

    // Try daemon save first (daemon merges metadata + cells from Automerge doc)
    // Clone handle out of the mutex to avoid holding lock across await
    let sync_handle = notebook_sync.lock().await.clone();
    let daemon_saved = if let Some(handle) = sync_handle {
        match handle
            .send_request(NotebookRequest::SaveNotebook {
                format_cells: false, // Already formatted above
            })
            .await
        {
            Ok(NotebookResponse::NotebookSaved {}) => {
                info!("[save] Notebook saved via daemon");
                true
            }
            Ok(NotebookResponse::Error { error }) => {
                warn!(
                    "[save] Daemon save failed: {}, falling back to local",
                    error
                );
                false
            }
            Ok(_) => {
                warn!("[save] Unexpected daemon response, falling back to local");
                false
            }
            Err(e) => {
                warn!("[save] Daemon request failed: {}, falling back to local", e);
                false
            }
        }
    } else {
        false
    };

    // Fallback: save locally if daemon save didn't work
    if !daemon_saved {
        let nb = state.lock().map_err(|e| e.to_string())?;
        let content = nb.serialize()?;
        std::fs::write(&path, &content).map_err(|e| e.to_string())?;
    }

    // Mark as clean
    {
        let mut nb = state.lock().map_err(|e| e.to_string())?;
        nb.dirty = false;
    }
    Ok(())
}

/// Save notebook to a specific path (Save As).
/// Formats all code cells before saving.
// TODO(automerge-metadata): Same as save_notebook — delegate disk write to the
// daemon via Automerge sync once the daemon owns persistence.
#[tauri::command]
async fn save_notebook_as(
    path: String,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let save_path = PathBuf::from(&path);

    // First pass: collect cells to format (release lock for async formatting)
    let (runtime, cells_to_format) = {
        let nb = state.lock().map_err(|e| e.to_string())?;
        let rt = nb.get_runtime();

        // Collect all code cells with their sources
        let cells: Vec<(String, String)> = nb
            .notebook
            .cells
            .iter()
            .filter_map(|cell| {
                if let nbformat::v4::Cell::Code { id, source, .. } = cell {
                    let src = source.join("");
                    if !src.trim().is_empty() {
                        return Some((id.to_string(), src));
                    }
                }
                None
            })
            .collect();

        (rt, cells)
    };

    // Format each cell (async, outside the lock)
    for (cell_id, source) in cells_to_format {
        let format_result = match runtime {
            Runtime::Python => format::format_python(&source).await,
            Runtime::Deno => format::format_deno(&source, "typescript").await,
            Runtime::Other(_) => Err(anyhow::anyhow!("No formatter for unknown runtime")),
        };

        if let Ok(result) = format_result {
            let cell_source = result.source_for_cell();
            if cell_source != source {
                // Update notebook state with formatted code
                {
                    let mut nb = state.lock().map_err(|e| e.to_string())?;
                    nb.update_cell_source(&cell_id, cell_source);
                }
                // Emit event to sync frontend
                let _ = emit_to_label::<_, _, _>(
                    &window,
                    window.label(),
                    "cell:source_updated",
                    serde_json::json!({
                        "cell_id": cell_id,
                        "source": cell_source,
                    }),
                );
            }
        }
    }

    // Now save
    let mut nb = state.lock().map_err(|e| e.to_string())?;
    let content = nb.serialize()?;
    std::fs::write(&save_path, &content).map_err(|e| e.to_string())?;

    // Update the stored path and window title
    let filename = save_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Untitled.ipynb");
    let _ = window.set_title(filename);

    nb.path = Some(save_path);
    nb.dirty = false;
    Ok(())
}

/// Clone the current notebook for saving as a new file.
/// Generates a fresh env_id and clears outputs/execution counts.
#[tauri::command]
async fn clone_notebook_to_path(
    path: String,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let notebook_state = notebook_state_for_window(&window, registry.inner())?;
    // Generate fresh env_id upfront
    let new_env_id = uuid::Uuid::new_v4().to_string();

    // Clone notebook structure while holding the lock
    let cloned_notebook = {
        let state = notebook_state.lock().map_err(|e| e.to_string())?;
        let mut cloned = state.notebook.clone();

        // Update runt metadata with new env_id (canonical location for env_id)
        if let Some(runt_value) = cloned.metadata.additional.get_mut("runt") {
            if let Some(obj) = runt_value.as_object_mut() {
                obj.insert("env_id".to_string(), serde_json::json!(new_env_id));
            }
        }

        // Clear outputs and execution counts from all code cells
        for cell in &mut cloned.cells {
            if let nbformat::v4::Cell::Code {
                outputs,
                execution_count,
                ..
            } = cell
            {
                outputs.clear();
                *execution_count = None;
            }
        }

        cloned
    };

    // Serialize and write to path
    let nb = nbformat::Notebook::V4(cloned_notebook);
    let content = nbformat::serialize_notebook(&nb).map_err(|e| e.to_string())?;
    std::fs::write(&path, &content).map_err(|e| e.to_string())?;

    Ok(())
}

/// Open a notebook file in a new window within the current app process.
#[tauri::command]
async fn open_notebook_in_new_window(
    path: String,
    app: tauri::AppHandle,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    open_notebook_window(&app, registry.inner(), Path::new(&path))
}

fn create_notebook_window(
    app: &tauri::AppHandle,
    registry: &WindowNotebookRegistry,
    state: NotebookState,
) -> Result<String, String> {
    let title = state
        .path
        .as_ref()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("Untitled.ipynb")
        .to_string();
    let label = format!("notebook-{}", uuid::Uuid::new_v4());
    let context = create_window_context(state);
    registry.insert(label.clone(), context.clone())?;

    let window =
        match tauri::WebviewWindowBuilder::new(app, label.clone(), tauri::WebviewUrl::default())
            .title(&title)
            .inner_size(1100.0, 750.0)
            .resizable(true)
            .build()
        {
            Ok(window) => window,
            Err(error) => {
                let mut contexts = registry.contexts.lock().map_err(|e| e.to_string())?;
                contexts.remove(&label);
                return Err(error.to_string());
            }
        };

    let context = registry.get(&label)?;
    tauri::async_runtime::spawn(async move {
        if let Err(e) =
            initialize_notebook_sync(window, context.notebook_state, context.notebook_sync).await
        {
            warn!("[startup] Notebook sync initialization failed: {}", e);
        }
    });

    Ok(label)
}

fn open_notebook_window(
    app: &tauri::AppHandle,
    registry: &WindowNotebookRegistry,
    path: &Path,
) -> Result<(), String> {
    let runtime = settings::load_settings().default_runtime;
    let state = load_notebook_state_for_path(path, runtime)?;
    create_notebook_window(app, registry, state).map(|_| ())
}

fn next_available_sample_path(base_dir: &Path, file_name: &str) -> PathBuf {
    let file_path = Path::new(file_name);
    let stem = file_path
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or("sample-notebook");
    let ext = file_path.extension().and_then(OsStr::to_str);

    let mut candidate = base_dir.join(file_name);
    let mut index = 2;

    while candidate.exists() {
        let next_name = match ext {
            Some(ext) => format!("{stem}-{index}.{ext}"),
            None => format!("{stem}-{index}"),
        };
        candidate = base_dir.join(next_name);
        index += 1;
    }

    candidate
}

fn materialize_sample_notebook(
    app: &tauri::AppHandle,
    sample: &crate::menu::BundledSampleNotebook,
) -> Result<PathBuf, String> {
    let base_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("Failed to resolve app data directory: {}", e))?
        .join("sample-notebooks");

    std::fs::create_dir_all(&base_dir)
        .map_err(|e| format!("Failed to create sample notebook directory: {}", e))?;

    let destination = next_available_sample_path(&base_dir, sample.file_name);
    std::fs::write(&destination, sample.contents)
        .map_err(|e| format!("Failed to write sample notebook: {}", e))?;

    Ok(destination)
}

fn open_bundled_sample_notebook(
    app: &tauri::AppHandle,
    registry: &WindowNotebookRegistry,
    sample: &crate::menu::BundledSampleNotebook,
) -> Result<(), String> {
    let path = materialize_sample_notebook(app, sample)?;
    open_notebook_window(app, registry, &path)
}

#[tauri::command]
async fn update_cell_source(
    cell_id: String,
    source: String,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    // Update local state synchronously for responsiveness
    {
        let mut s = state.lock().map_err(|e| e.to_string())?;
        s.update_cell_source(&cell_id, &source);
    }

    // Sync to daemon (fire-and-forget errors to maintain responsiveness)
    let guard = notebook_sync.lock().await;
    if let Some(handle) = guard.as_ref() {
        info!("[notebook-sync] Syncing source update for cell {}", cell_id);
        if let Err(e) = handle.update_source(&cell_id, &source).await {
            warn!("[notebook-sync] update_source failed: {}", e);
        }
    } else {
        info!("[notebook-sync] No sync handle available for update_source");
    }

    Ok(())
}

#[tauri::command]
async fn add_cell(
    cell_type: String,
    after_cell_id: Option<String>,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<FrontendCell, String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    // Add to local state first
    let (cell, index) = {
        let mut s = state.lock().map_err(|e| e.to_string())?;

        // Find the index where the new cell will be inserted
        let insert_index = match &after_cell_id {
            Some(id) => s.find_cell_index(id).map(|i| i + 1).unwrap_or(0),
            None => 0,
        };

        let cell = s
            .add_cell(&cell_type, after_cell_id.as_deref())
            .ok_or_else(|| format!("Invalid cell type: {}", cell_type))?;

        (cell, insert_index)
    };

    // Sync to daemon
    let guard = notebook_sync.lock().await;
    if let Some(handle) = guard.as_ref() {
        let cell_id = match &cell {
            FrontendCell::Code { id, .. } => id,
            FrontendCell::Markdown { id, .. } => id,
            FrontendCell::Raw { id, .. } => id,
        };
        info!(
            "[notebook-sync] Syncing add_cell {} at index {}",
            cell_id, index
        );
        if let Err(e) = handle.add_cell(index, cell_id, &cell_type).await {
            warn!("[notebook-sync] add_cell failed: {}", e);
        }
    } else {
        info!("[notebook-sync] No sync handle available for add_cell");
    }

    Ok(cell)
}

#[tauri::command]
async fn delete_cell(
    cell_id: String,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    // Delete from local state first
    {
        let mut s = state.lock().map_err(|e| e.to_string())?;
        if !s.delete_cell(&cell_id) {
            return Err("Cannot delete cell (last cell or not found)".to_string());
        }
    }

    // Sync to daemon
    if let Some(handle) = notebook_sync.lock().await.as_ref() {
        if let Err(e) = handle.delete_cell(&cell_id).await {
            warn!("[notebook-sync] delete_cell failed: {}", e);
        }
    }

    Ok(())
}

// ============================================================================
// Daemon Kernel Operations
// ============================================================================
// These commands route kernel operations through the daemon, which owns the
// kernel lifecycle and execution queue. This enables multi-window kernel sharing.

/// Launch a kernel via the daemon.
///
/// If a kernel is already running for this notebook, returns info about the existing kernel.
/// The notebook_path is automatically derived from the sync handle if not provided.
#[tauri::command]
async fn launch_kernel_via_daemon(
    kernel_type: String,
    env_source: String,
    notebook_path: Option<String>,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<NotebookResponse, String> {
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;

    // Use notebook_id from the sync handle if notebook_path not provided,
    // but only if it looks like a real file path (not a UUID for untitled notebooks)
    let resolved_path = notebook_path.or_else(|| {
        let id = handle.notebook_id();
        // Check if it looks like a file path (contains path separator or starts with /)
        if id.contains('/') || id.contains('\\') {
            Some(id.to_string())
        } else {
            // Likely a UUID for an untitled notebook - don't use as path
            None
        }
    });

    info!(
        "[daemon-kernel] launch_kernel_via_daemon: type={}, env_source={}, path={:?}",
        kernel_type, env_source, resolved_path
    );

    handle
        .send_request(NotebookRequest::LaunchKernel {
            kernel_type,
            env_source,
            notebook_path: resolved_path,
        })
        .await
        .map_err(|e| format!("daemon request failed: {}", e))
}

/// Queue a cell for execution via the daemon.
///
/// The daemon manages the execution queue and broadcasts outputs to all windows.
#[tauri::command]
#[allow(deprecated)]
async fn queue_cell_via_daemon(
    cell_id: String,
    code: String,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<NotebookResponse, String> {
    info!("[daemon-kernel] queue_cell_via_daemon: cell_id={}", cell_id);

    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;

    handle
        .send_request(NotebookRequest::QueueCell { cell_id, code })
        .await
        .map_err(|e| format!("daemon request failed: {}", e))
}

/// Execute a cell via the daemon (reads source from synced document).
///
/// This is the preferred method - ensures execution matches synced document state.
/// The daemon reads the cell source from the Automerge doc instead of receiving it as a parameter.
#[tauri::command]
async fn execute_cell_via_daemon(
    cell_id: String,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<NotebookResponse, String> {
    info!(
        "[daemon-kernel] execute_cell_via_daemon: cell_id={}",
        cell_id
    );

    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;

    handle
        .send_request(NotebookRequest::ExecuteCell { cell_id })
        .await
        .map_err(|e| format!("daemon request failed: {}", e))
}

/// Clear outputs for a cell via the daemon.
#[tauri::command]
async fn clear_outputs_via_daemon(
    cell_id: String,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<NotebookResponse, String> {
    info!(
        "[daemon-kernel] clear_outputs_via_daemon: cell_id={}",
        cell_id
    );

    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;

    handle
        .send_request(NotebookRequest::ClearOutputs { cell_id })
        .await
        .map_err(|e| format!("daemon request failed: {}", e))
}

/// Interrupt kernel execution via the daemon.
#[tauri::command]
async fn interrupt_via_daemon(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<NotebookResponse, String> {
    info!("[daemon-kernel] interrupt_via_daemon");

    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;

    handle
        .send_request(NotebookRequest::InterruptExecution {})
        .await
        .map_err(|e| format!("daemon request failed: {}", e))
}

/// Shutdown the kernel via the daemon.
#[tauri::command]
async fn shutdown_kernel_via_daemon(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<NotebookResponse, String> {
    info!("[daemon-kernel] shutdown_kernel_via_daemon");

    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;

    handle
        .send_request(NotebookRequest::ShutdownKernel {})
        .await
        .map_err(|e| format!("daemon request failed: {}", e))
}

/// Get kernel info from the daemon.
#[tauri::command]
async fn get_daemon_kernel_info(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<NotebookResponse, String> {
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let guard = notebook_sync.lock().await;
    let has_handle = guard.is_some();
    info!(
        "[daemon-kernel] get_daemon_kernel_info called (has_handle: {})",
        has_handle
    );

    let handle = guard.as_ref().ok_or_else(|| {
        warn!("[daemon-kernel] get_daemon_kernel_info: notebook_sync is None - connection may have failed or been cleared");
        "Not connected to daemon".to_string()
    })?;

    handle
        .send_request(NotebookRequest::GetKernelInfo {})
        .await
        .map_err(|e| format!("daemon request failed: {}", e))
}

/// Check if daemon is connected.
/// Returns true if notebook_sync handle exists (daemon available).
#[tauri::command]
async fn is_daemon_connected(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<bool, String> {
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let guard = notebook_sync.lock().await;
    Ok(guard.is_some())
}

/// Get execution queue state from the daemon.
#[tauri::command]
async fn get_daemon_queue_state(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<NotebookResponse, String> {
    info!("[daemon-kernel] get_daemon_queue_state");

    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;

    handle
        .send_request(NotebookRequest::GetQueueState {})
        .await
        .map_err(|e| format!("daemon request failed: {}", e))
}

/// Run all code cells via the daemon.
/// Daemon reads cell sources from the synced Automerge document.
#[tauri::command]
async fn run_all_cells_via_daemon(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<NotebookResponse, String> {
    info!("[daemon-kernel] run_all_cells_via_daemon");

    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;

    handle
        .send_request(NotebookRequest::RunAllCells {})
        .await
        .map_err(|e| format!("daemon request failed: {}", e))
}

/// Send a comm message to the kernel via the daemon (for widget interactions).
///
/// Accepts the full Jupyter message envelope to preserve header/session for
/// proper widget protocol compliance.
#[tauri::command]
async fn send_comm_via_daemon(
    message: serde_json::Value,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<NotebookResponse, String> {
    let msg_type = message
        .get("header")
        .and_then(|h| h.get("msg_type"))
        .and_then(|t| t.as_str())
        .unwrap_or("unknown");
    info!(
        "[daemon-kernel] send_comm_via_daemon: msg_type={}",
        msg_type
    );

    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;

    handle
        .send_request(NotebookRequest::SendComm { message })
        .await
        .map_err(|e| format!("daemon request failed: {}", e))
}

/// Get kernel input history via daemon.
///
/// Searches the kernel's input history for matching entries.
/// Returns an error if no kernel is running or the request times out.
#[tauri::command]
async fn get_history_via_daemon(
    pattern: Option<String>,
    n: i32,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<Vec<HistoryEntry>, String> {
    info!(
        "[daemon-kernel] get_history_via_daemon: pattern={:?}, n={}",
        pattern, n
    );

    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;

    let response = handle
        .send_request(NotebookRequest::GetHistory {
            pattern,
            n,
            unique: true,
        })
        .await
        .map_err(|e| format!("daemon request failed: {}", e))?;

    match response {
        NotebookResponse::HistoryResult { entries } => Ok(entries),
        NotebookResponse::NoKernel {} => Err("No kernel running".to_string()),
        NotebookResponse::Error { error } => Err(error),
        _ => Err("Unexpected response from daemon".to_string()),
    }
}

/// Result type for completion requests (matches frontend interface).
#[derive(Serialize)]
struct CompletionResult {
    items: Vec<CompletionItem>,
    cursor_start: usize,
    cursor_end: usize,
}

/// Get code completions via daemon.
///
/// Requests completions from the kernel for the given code at cursor position.
/// Returns an error if no kernel is running or the request times out.
#[tauri::command]
async fn complete_via_daemon(
    code: String,
    cursor_pos: usize,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<CompletionResult, String> {
    debug!(
        "[daemon-kernel] complete_via_daemon: cursor_pos={}",
        cursor_pos
    );

    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;

    let response = handle
        .send_request(NotebookRequest::Complete { code, cursor_pos })
        .await
        .map_err(|e| format!("daemon request failed: {}", e))?;

    match response {
        NotebookResponse::CompletionResult {
            items,
            cursor_start,
            cursor_end,
        } => Ok(CompletionResult {
            items,
            cursor_start,
            cursor_end,
        }),
        NotebookResponse::NoKernel {} => Err("No kernel running".to_string()),
        NotebookResponse::Error { error } => Err(error),
        _ => Err("Unexpected response from daemon".to_string()),
    }
}

/// Reconnect to the daemon after a disconnection.
///
/// Called by the frontend after receiving daemon:disconnected event.
#[tauri::command]
async fn reconnect_to_daemon(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
    reconnect_in_progress: tauri::State<'_, ReconnectInProgress>,
) -> Result<(), String> {
    info!("[daemon-kernel] reconnect_to_daemon");

    let notebook_state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;

    // Use atomic compare_exchange to ensure only one reconnect runs at a time
    if reconnect_in_progress
        .0
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        info!("[daemon-kernel] Reconnect already in progress, skipping");
        return Ok(());
    }

    // Helper to reset flag on all exit paths
    let reset_flag = || reconnect_in_progress.0.store(false, Ordering::SeqCst);

    // Check if already connected
    {
        let sync_guard = notebook_sync.lock().await;
        if sync_guard.is_some() {
            info!("[daemon-kernel] Already connected to daemon");
            reset_flag();
            return Ok(());
        }
    }

    // Re-initialize notebook sync
    let webview_window = window
        .app_handle()
        .get_webview_window(window.label())
        .ok_or_else(|| "Current webview window not found".to_string())?;
    let result = initialize_notebook_sync(webview_window, notebook_state, notebook_sync).await;

    reset_flag();
    result
}

/// Refresh cells from Automerge and emit notebook:updated event.
///
/// Used by the frontend to request the current Automerge state after
/// setting up listeners (handles race condition where initial state
/// was emitted before listeners were ready).
#[tauri::command]
async fn refresh_from_automerge(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;

    let cells = handle
        .get_cells()
        .await
        .map_err(|e| format!("Failed to get cells: {}", e))?;

    info!(
        "[notebook-sync] Refreshing frontend with {} cells from Automerge",
        cells.len()
    );

    // Emit to frontend (which will resolve manifest hashes)
    emit_to_label::<_, _, _>(&window, window.label(), "notebook:updated", &cells)
        .map_err(|e| format!("Failed to emit notebook:updated: {}", e))
}

/// Debug: Get Automerge document state from the daemon.
///
/// Returns the cells as the daemon sees them, useful for debugging sync issues.
#[tauri::command]
async fn debug_get_automerge_state(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<Vec<serde_json::Value>, String> {
    info!("[debug] Getting Automerge state from daemon");

    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;

    let cells = handle
        .get_cells()
        .await
        .map_err(|e| format!("Failed to get cells: {}", e))?;

    // Convert CellSnapshots to JSON for easy inspection
    let json_cells: Vec<serde_json::Value> = cells
        .into_iter()
        .map(|cell| {
            serde_json::json!({
                "id": cell.id,
                "cell_type": cell.cell_type,
                "source": cell.source,
                "execution_count": cell.execution_count,
                "outputs_count": cell.outputs.len(),
                "outputs": cell.outputs,
            })
        })
        .collect();

    Ok(json_cells)
}

/// Debug: Get local notebook state (in-memory).
#[tauri::command]
fn debug_get_local_state(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<Vec<serde_json::Value>, String> {
    info!("[debug] Getting local notebook state");

    let state = notebook_state_for_window(&window, registry.inner())?;
    let state = state.lock().map_err(|e| e.to_string())?;

    // Use cells_for_frontend which handles the nbformat Cell enum
    let frontend_cells = state.cells_for_frontend();

    let json_cells: Vec<serde_json::Value> = frontend_cells
        .into_iter()
        .map(|cell| match cell {
            FrontendCell::Code {
                id,
                source,
                outputs,
                execution_count,
            } => serde_json::json!({
                "id": id,
                "cell_type": "code",
                "source": source,
                "execution_count": execution_count,
                "outputs_count": outputs.len(),
            }),
            FrontendCell::Markdown { id, source } => serde_json::json!({
                "id": id,
                "cell_type": "markdown",
                "source": source,
            }),
            FrontendCell::Raw { id, source } => serde_json::json!({
                "id": id,
                "cell_type": "raw",
                "source": source,
            }),
        })
        .collect();

    Ok(json_cells)
}

#[tauri::command]
async fn get_preferred_kernelspec(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<Option<String>, String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let state = state.lock().map_err(|e| e.to_string())?;
    Ok(state
        .notebook
        .metadata
        .kernelspec
        .as_ref()
        .map(|k| k.name.clone()))
}

#[tauri::command]
async fn list_kernelspecs() -> Result<Vec<KernelspecInfo>, String> {
    let specs = runtimelib::list_kernelspecs().await;
    Ok(specs
        .into_iter()
        .map(|s| KernelspecInfo {
            name: s.kernel_name,
            display_name: s.kernelspec.display_name,
            language: s.kernelspec.language,
        })
        .collect())
}

// ============================================================================
// UV Dependency Management Commands
// ============================================================================

/// Serializable notebook dependencies for the frontend.
#[derive(Serialize, Deserialize, Clone)]
struct NotebookDependenciesJson {
    dependencies: Vec<String>,
    requires_python: Option<String>,
}

/// Check if uv is available on the system.
#[tauri::command]
async fn check_uv_available() -> bool {
    uv_env::check_uv_available().await
}

/// Get dependencies from notebook metadata.
#[tauri::command]
async fn get_notebook_dependencies(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<Option<NotebookDependenciesJson>, String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let state = state.lock().map_err(|e| e.to_string())?;
    let deps = uv_env::extract_dependencies(&state.notebook.metadata);
    Ok(deps.map(|d| NotebookDependenciesJson {
        dependencies: d.dependencies,
        requires_python: d.requires_python,
    }))
}

/// Set dependencies in notebook metadata.
#[tauri::command]
async fn set_notebook_dependencies(
    dependencies: Vec<String>,
    requires_python: Option<String>,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    {
        let mut state = state.lock().map_err(|e| e.to_string())?;
        let deps = uv_env::NotebookDependencies {
            dependencies,
            requires_python,
        };
        uv_env::set_dependencies(&mut state.notebook.metadata, &deps);
        state.dirty = true;
    }
    push_metadata_to_sync(&state, &notebook_sync).await;
    Ok(())
}

/// Add a single dependency to the notebook.
#[tauri::command]
async fn add_dependency(
    package: String,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let changed = {
        let mut s = state.lock().map_err(|e| e.to_string())?;

        // Get existing deps or create new
        let existing = uv_env::extract_dependencies(&s.notebook.metadata);
        let mut deps = existing
            .as_ref()
            .map(|d| d.dependencies.clone())
            .unwrap_or_default();

        // Check if already exists (by package name, ignoring version specifiers)
        let pkg_name = package
            .split(&['>', '<', '=', '!', '~', '['][..])
            .next()
            .unwrap_or(&package);
        let already_exists = deps.iter().any(|d| {
            let existing_name = d
                .split(&['>', '<', '=', '!', '~', '['][..])
                .next()
                .unwrap_or(d);
            existing_name.eq_ignore_ascii_case(pkg_name)
        });

        if !already_exists {
            deps.push(package);
            let requires_python = existing.and_then(|d| d.requires_python);
            let new_deps = uv_env::NotebookDependencies {
                dependencies: deps,
                requires_python,
            };
            uv_env::set_dependencies(&mut s.notebook.metadata, &new_deps);
            s.dirty = true;
            true
        } else {
            false
        }
    };
    if changed {
        push_metadata_to_sync(&state, &notebook_sync).await;
    }
    Ok(())
}

/// Remove a dependency from the notebook.
#[tauri::command]
async fn remove_dependency(
    package: String,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    {
        let mut s = state.lock().map_err(|e| e.to_string())?;
        let existing = uv_env::extract_dependencies(&s.notebook.metadata);
        if let Some(existing) = existing {
            let pkg_name = package
                .split(&['>', '<', '=', '!', '~', '['][..])
                .next()
                .unwrap_or(&package);
            let deps: Vec<String> = existing
                .dependencies
                .into_iter()
                .filter(|d| {
                    let existing_name = d
                        .split(&['>', '<', '=', '!', '~', '['][..])
                        .next()
                        .unwrap_or(d);
                    !existing_name.eq_ignore_ascii_case(pkg_name)
                })
                .collect();
            let new_deps = uv_env::NotebookDependencies {
                dependencies: deps,
                requires_python: existing.requires_python,
            };
            uv_env::set_dependencies(&mut s.notebook.metadata, &new_deps);
            s.dirty = true;
        }
    }
    push_metadata_to_sync(&state, &notebook_sync).await;
    Ok(())
}

/// Remove an entire dependency metadata section ("uv" or "conda") from the notebook.
///
/// Used when a notebook has both uv and conda inline dependencies and the user
/// chooses which one to keep — the other section is removed entirely.
#[tauri::command]
async fn clear_dependency_section(
    section: String,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    if section != "uv" && section != "conda" {
        return Err(format!(
            "Invalid section: {}. Must be 'uv' or 'conda'.",
            section
        ));
    }

    {
        let mut s = state.lock().map_err(|e| e.to_string())?;
        match section.as_str() {
            "uv" => {
                if uv_env::has_uv_config(&s.notebook.metadata) {
                    uv_env::remove_uv_config(&mut s.notebook.metadata);
                    s.dirty = true;
                }
            }
            "conda" => {
                if conda_env::has_conda_config(&s.notebook.metadata) {
                    conda_env::remove_conda_config(&mut s.notebook.metadata);
                    s.dirty = true;
                }
            }
            _ => {}
        }
    }
    push_metadata_to_sync(&state, &notebook_sync).await;
    Ok(())
}

// ============================================================================
// Conda Dependency Management Commands
// ============================================================================

/// Serializable conda notebook dependencies for the frontend.
#[derive(Serialize, Deserialize, Clone)]
struct CondaDependenciesJson {
    dependencies: Vec<String>,
    channels: Vec<String>,
    python: Option<String>,
}

/// Get conda dependencies from notebook metadata.
#[tauri::command]
async fn get_conda_dependencies(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<Option<CondaDependenciesJson>, String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let state = state.lock().map_err(|e| e.to_string())?;
    let deps = conda_env::extract_dependencies(&state.notebook.metadata);
    Ok(deps.map(|d| CondaDependenciesJson {
        dependencies: d.dependencies,
        channels: d.channels,
        python: d.python,
    }))
}

/// Set conda dependencies in notebook metadata.
#[tauri::command]
async fn set_conda_dependencies(
    dependencies: Vec<String>,
    channels: Vec<String>,
    python: Option<String>,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    {
        let mut s = state.lock().map_err(|e| e.to_string())?;
        let deps = conda_env::CondaDependencies {
            dependencies,
            channels,
            python,
            env_id: None,
        };
        conda_env::set_dependencies(&mut s.notebook.metadata, &deps);
        s.dirty = true;
    }
    push_metadata_to_sync(&state, &notebook_sync).await;
    Ok(())
}

/// Add a single conda dependency to the notebook.
#[tauri::command]
async fn add_conda_dependency(
    package: String,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let changed = {
        let mut s = state.lock().map_err(|e| e.to_string())?;

        let existing = conda_env::extract_dependencies(&s.notebook.metadata);
        let mut deps = existing
            .as_ref()
            .map(|d| d.dependencies.clone())
            .unwrap_or_default();
        let channels = existing
            .as_ref()
            .map(|d| d.channels.clone())
            .unwrap_or_default();
        let python = existing.as_ref().and_then(|d| d.python.clone());

        let pkg_name = package
            .split(&['>', '<', '=', '!', '~', '['][..])
            .next()
            .unwrap_or(&package);
        let already_exists = deps.iter().any(|d| {
            let existing_name = d
                .split(&['>', '<', '=', '!', '~', '['][..])
                .next()
                .unwrap_or(d);
            existing_name.eq_ignore_ascii_case(pkg_name)
        });

        if !already_exists {
            deps.push(package);
            let new_deps = conda_env::CondaDependencies {
                dependencies: deps,
                channels,
                python,
                env_id: None,
            };
            conda_env::set_dependencies(&mut s.notebook.metadata, &new_deps);
            s.dirty = true;
            true
        } else {
            false
        }
    };
    if changed {
        push_metadata_to_sync(&state, &notebook_sync).await;
    }
    Ok(())
}

/// Remove a conda dependency from the notebook.
#[tauri::command]
async fn remove_conda_dependency(
    package: String,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    {
        let mut s = state.lock().map_err(|e| e.to_string())?;
        let existing = conda_env::extract_dependencies(&s.notebook.metadata);
        if let Some(existing) = existing {
            let pkg_name = package
                .split(&['>', '<', '=', '!', '~', '['][..])
                .next()
                .unwrap_or(&package);
            let deps: Vec<String> = existing
                .dependencies
                .into_iter()
                .filter(|d| {
                    let existing_name = d
                        .split(&['>', '<', '=', '!', '~', '['][..])
                        .next()
                        .unwrap_or(d);
                    !existing_name.eq_ignore_ascii_case(pkg_name)
                })
                .collect();
            let new_deps = conda_env::CondaDependencies {
                dependencies: deps,
                channels: existing.channels,
                python: existing.python,
                env_id: existing.env_id,
            };
            conda_env::set_dependencies(&mut s.notebook.metadata, &new_deps);
            s.dirty = true;
        }
    }
    push_metadata_to_sync(&state, &notebook_sync).await;
    Ok(())
}

// ============================================================================
// pyproject.toml Discovery and Environment Commands
// ============================================================================

/// Detect pyproject.toml near the notebook and return info about it.
#[tauri::command]
async fn detect_pyproject(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<Option<pyproject::PyProjectInfo>, String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_path = {
        let state = state.lock().map_err(|e| e.to_string())?;
        state.path.clone()
    };

    // Need a notebook path to search from
    let Some(notebook_path) = notebook_path else {
        return Ok(None);
    };

    // Find pyproject.toml walking up from notebook directory
    let Some(pyproject_path) = pyproject::find_pyproject(&notebook_path) else {
        return Ok(None);
    };

    // Parse and create info
    let config = pyproject::parse_pyproject(&pyproject_path).map_err(|e| e.to_string())?;
    let info = pyproject::create_pyproject_info(&config, &notebook_path);

    info!(
        "Detected pyproject.toml at {} with {} dependencies",
        info.relative_path, info.dependency_count
    );

    Ok(Some(info))
}

/// Full pyproject dependencies for display in the UI.
#[derive(Serialize)]
struct PyProjectDepsJson {
    path: String,
    relative_path: String,
    project_name: Option<String>,
    dependencies: Vec<String>,
    dev_dependencies: Vec<String>,
    requires_python: Option<String>,
    index_url: Option<String>,
}

/// Get full parsed dependencies from the detected pyproject.toml.
#[tauri::command]
async fn get_pyproject_dependencies(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<Option<PyProjectDepsJson>, String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_path = {
        let state = state.lock().map_err(|e| e.to_string())?;
        state.path.clone()
    };

    let Some(notebook_path) = notebook_path else {
        return Ok(None);
    };

    let Some(pyproject_path) = pyproject::find_pyproject(&notebook_path) else {
        return Ok(None);
    };

    let config = pyproject::parse_pyproject(&pyproject_path).map_err(|e| e.to_string())?;

    let relative_path = pathdiff::diff_paths(
        &config.path,
        notebook_path.parent().unwrap_or(&notebook_path),
    )
    .map(|p| p.display().to_string())
    .unwrap_or_else(|| config.path.display().to_string());

    Ok(Some(PyProjectDepsJson {
        path: config.path.display().to_string(),
        relative_path,
        project_name: config.project_name,
        dependencies: config.dependencies,
        dev_dependencies: config.dev_dependencies,
        requires_python: config.requires_python,
        index_url: config.index_url,
    }))
}

/// Import dependencies from pyproject.toml into notebook metadata.
/// This makes the notebook more portable.
#[tauri::command]
async fn import_pyproject_dependencies(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let notebook_path = {
        let s = state.lock().map_err(|e| e.to_string())?;
        s.path.clone()
    };

    let Some(notebook_path) = notebook_path else {
        return Err("No notebook path set".to_string());
    };

    let Some(pyproject_path) = pyproject::find_pyproject(&notebook_path) else {
        return Err("No pyproject.toml found".to_string());
    };

    let config = pyproject::parse_pyproject(&pyproject_path).map_err(|e| e.to_string())?;

    {
        let mut s = state.lock().map_err(|e| e.to_string())?;
        let all_deps = pyproject::get_all_dependencies(&config);
        let deps = uv_env::NotebookDependencies {
            dependencies: all_deps.clone(),
            requires_python: config.requires_python,
        };
        uv_env::set_dependencies(&mut s.notebook.metadata, &deps);
        s.dirty = true;
        info!(
            "Imported {} dependencies from pyproject.toml into notebook",
            all_deps.len()
        );
    }
    push_metadata_to_sync(&state, &notebook_sync).await;
    Ok(())
}

// ============================================================================
// Trust Verification Commands
// ============================================================================

/// Verify the trust status of the current notebook's dependencies.
///
/// Returns the trust status and information about what packages would be installed.
#[tauri::command]
async fn verify_notebook_trust(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<trust::TrustInfo, String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let state = state.lock().map_err(|e| e.to_string())?;
    trust::verify_notebook_trust(&state.notebook.metadata.additional)
}

/// Approve the notebook's dependencies and sign them with the local trust key.
///
/// After calling this, the notebook will be trusted on subsequent opens (until
/// the dependency metadata is modified externally).
#[tauri::command]
async fn approve_notebook_trust(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    {
        let mut s = state.lock().map_err(|e| e.to_string())?;

        // Compute signature over current dependencies
        let signature = trust::sign_notebook_dependencies(&s.notebook.metadata.additional)?;

        // Get or create the runt metadata section
        let runt_value = s
            .notebook
            .metadata
            .additional
            .entry("runt".to_string())
            .or_insert_with(|| serde_json::json!({}));

        // Add/update the trust signature
        if let Some(obj) = runt_value.as_object_mut() {
            obj.insert(
                "trust_signature".to_string(),
                serde_json::Value::String(signature),
            );
            obj.insert(
                "trust_timestamp".to_string(),
                serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
            );
        }

        s.dirty = true;
    }
    push_metadata_to_sync(&state, &notebook_sync).await;
    Ok(())
}

/// Check packages for typosquatting (similar names to popular packages).
///
/// Returns warnings for any packages that look like potential typosquats.
#[tauri::command]
async fn check_typosquats(packages: Vec<String>) -> Vec<typosquat::TyposquatWarning> {
    typosquat::check_packages(&packages)
}

// ============================================================================
// pixi.toml Discovery and Environment Commands
// ============================================================================

/// Detect pixi.toml near the notebook and return info about it.
#[tauri::command]
async fn detect_pixi_toml(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<Option<pixi::PixiInfo>, String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_path = {
        let state = state.lock().map_err(|e| e.to_string())?;
        state.path.clone()
    };

    // Need a notebook path to search from
    let Some(notebook_path) = notebook_path else {
        return Ok(None);
    };

    // Find pixi.toml walking up from notebook directory
    let Some(pixi_path) = pixi::find_pixi_toml(&notebook_path) else {
        return Ok(None);
    };

    // Parse and create info
    let config = pixi::parse_pixi_toml(&pixi_path).map_err(|e| e.to_string())?;
    let info = pixi::create_pixi_info(&config, &notebook_path);

    info!(
        "Detected pixi.toml at {} with {} dependencies",
        info.relative_path, info.dependency_count
    );

    Ok(Some(info))
}

/// Full pixi dependencies for display in the UI.
#[derive(Serialize)]
struct PixiDepsJson {
    path: String,
    relative_path: String,
    workspace_name: Option<String>,
    dependencies: Vec<String>,
    pypi_dependencies: Vec<String>,
    python: Option<String>,
    channels: Vec<String>,
}

/// Get full parsed dependencies from the detected pixi.toml.
#[tauri::command]
async fn get_pixi_dependencies(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<Option<PixiDepsJson>, String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_path = {
        let state = state.lock().map_err(|e| e.to_string())?;
        state.path.clone()
    };

    let Some(notebook_path) = notebook_path else {
        return Ok(None);
    };

    let Some(pixi_path) = pixi::find_pixi_toml(&notebook_path) else {
        return Ok(None);
    };

    let config = pixi::parse_pixi_toml(&pixi_path).map_err(|e| e.to_string())?;

    let relative_path = pathdiff::diff_paths(
        &config.path,
        notebook_path.parent().unwrap_or(&notebook_path),
    )
    .map(|p| p.display().to_string())
    .unwrap_or_else(|| config.path.display().to_string());

    Ok(Some(PixiDepsJson {
        path: config.path.display().to_string(),
        relative_path,
        workspace_name: config.workspace_name,
        dependencies: config.dependencies,
        pypi_dependencies: config.pypi_dependencies,
        python: config.python,
        channels: config.channels,
    }))
}

// ============================================================================
// environment.yml Discovery and Environment Commands
// ============================================================================

/// Detect environment.yml near the notebook and return info about it.
#[tauri::command]
async fn detect_environment_yml(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<Option<environment_yml::EnvironmentYmlInfo>, String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_path = {
        let state = state.lock().map_err(|e| e.to_string())?;
        state.path.clone()
    };

    // Need a notebook path to search from
    let Some(notebook_path) = notebook_path else {
        return Ok(None);
    };

    // Find environment.yml walking up from notebook directory
    let Some(yml_path) = environment_yml::find_environment_yml(&notebook_path) else {
        return Ok(None);
    };

    // Parse and create info
    let config = environment_yml::parse_environment_yml(&yml_path).map_err(|e| e.to_string())?;
    let info = environment_yml::create_environment_yml_info(&config, &notebook_path);

    info!(
        "Detected environment.yml at {} with {} dependencies",
        info.relative_path, info.dependency_count
    );

    Ok(Some(info))
}

/// Full environment.yml dependencies for display in the UI.
#[derive(Serialize)]
struct EnvironmentYmlDepsJson {
    path: String,
    relative_path: String,
    name: Option<String>,
    dependencies: Vec<String>,
    pip_dependencies: Vec<String>,
    python: Option<String>,
    channels: Vec<String>,
}

/// Get full parsed dependencies from the detected environment.yml.
#[tauri::command]
async fn get_environment_yml_dependencies(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<Option<EnvironmentYmlDepsJson>, String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_path = {
        let state = state.lock().map_err(|e| e.to_string())?;
        state.path.clone()
    };

    let Some(notebook_path) = notebook_path else {
        return Ok(None);
    };

    let Some(yml_path) = environment_yml::find_environment_yml(&notebook_path) else {
        return Ok(None);
    };

    let config = environment_yml::parse_environment_yml(&yml_path).map_err(|e| e.to_string())?;

    let relative_path = pathdiff::diff_paths(
        &config.path,
        notebook_path.parent().unwrap_or(&notebook_path),
    )
    .map(|p| p.display().to_string())
    .unwrap_or_else(|| config.path.display().to_string());

    Ok(Some(EnvironmentYmlDepsJson {
        path: config.path.display().to_string(),
        relative_path,
        name: config.name,
        dependencies: config.dependencies,
        pip_dependencies: config.pip_dependencies,
        python: config.python,
        channels: config.channels,
    }))
}

/// Import dependencies from pixi.toml into notebook conda metadata.
/// This converts pixi deps to conda format and stores them inline in the notebook.
#[tauri::command]
async fn import_pixi_dependencies(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let notebook_path = {
        let s = state.lock().map_err(|e| e.to_string())?;
        s.path.clone()
    };

    let Some(notebook_path) = notebook_path else {
        return Err("No notebook path set".to_string());
    };

    let Some(pixi_path) = pixi::find_pixi_toml(&notebook_path) else {
        return Err("No pixi.toml found".to_string());
    };

    let config = pixi::parse_pixi_toml(&pixi_path).map_err(|e| e.to_string())?;
    let conda_deps = pixi::convert_to_conda_dependencies(&config);

    {
        let mut s = state.lock().map_err(|e| e.to_string())?;
        let deps = conda_env::CondaDependencies {
            dependencies: conda_deps.dependencies.clone(),
            channels: conda_deps.channels,
            python: conda_deps.python,
            env_id: None,
        };
        conda_env::set_dependencies(&mut s.notebook.metadata, &deps);
        s.dirty = true;
        info!(
            "Imported {} dependencies from pixi.toml into notebook conda metadata",
            conda_deps.dependencies.len()
        );
    }
    push_metadata_to_sync(&state, &notebook_sync).await;
    Ok(())
}

// ========== Deno kernel support ==========

/// Check if Deno is available on the system
#[tauri::command]
async fn check_deno_available() -> bool {
    deno_env::check_deno_available().await
}

/// Get the installed Deno version
#[tauri::command]
async fn get_deno_version() -> Result<String, String> {
    deno_env::get_deno_version()
        .await
        .map_err(|e| e.to_string())
}

/// Get the runtime type from notebook metadata
#[tauri::command]
async fn get_notebook_runtime(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<String, String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let state = state.lock().map_err(|e| e.to_string())?;
    Ok(state.get_runtime().to_string())
}

/// Detect deno.json/deno.jsonc near the notebook and return info about it
#[tauri::command]
async fn detect_deno_config(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<Option<deno_env::DenoConfigInfo>, String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_path = {
        let state = state.lock().map_err(|e| e.to_string())?;
        state.path.clone()
    };

    let Some(notebook_path) = notebook_path else {
        return Ok(None);
    };

    let Some(config_path) = deno_env::find_deno_config(&notebook_path) else {
        return Ok(None);
    };

    let config = deno_env::parse_deno_config(&config_path).map_err(|e| e.to_string())?;
    Ok(Some(deno_env::create_deno_config_info(
        &config,
        &notebook_path,
    )))
}

/// Get Deno permissions from notebook metadata
#[tauri::command]
async fn get_deno_permissions(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<Vec<String>, String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let state = state.lock().map_err(|e| e.to_string())?;
    let deps = deno_env::extract_deno_metadata(&state.notebook.metadata);
    Ok(deps.map(|d| d.permissions).unwrap_or_default())
}

/// Set Deno permissions in notebook metadata
#[tauri::command]
async fn set_deno_permissions(
    permissions: Vec<String>,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    {
        let mut s = state.lock().map_err(|e| e.to_string())?;
        let mut deno_deps =
            deno_env::extract_deno_metadata(&s.notebook.metadata).unwrap_or_default();
        deno_deps.permissions = permissions;
        let deno_value = serde_json::to_value(&deno_deps).map_err(|e| e.to_string())?;
        s.notebook
            .metadata
            .additional
            .insert("deno".to_string(), deno_value);
        s.dirty = true;
    }
    push_metadata_to_sync(&state, &notebook_sync).await;
    Ok(())
}

/// Get Deno flexible npm imports setting from notebook metadata
#[tauri::command]
async fn get_deno_flexible_npm_imports(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<bool, String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let state = state.lock().map_err(|e| e.to_string())?;
    let deps = deno_env::extract_deno_metadata(&state.notebook.metadata);
    Ok(deps.map(|d| d.flexible_npm_imports).unwrap_or(true))
}

/// Set Deno flexible npm imports setting in notebook metadata
#[tauri::command]
async fn set_deno_flexible_npm_imports(
    enabled: bool,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    {
        let mut s = state.lock().map_err(|e| e.to_string())?;
        let mut deno_deps =
            deno_env::extract_deno_metadata(&s.notebook.metadata).unwrap_or_default();
        deno_deps.flexible_npm_imports = enabled;
        let deno_value = serde_json::to_value(&deno_deps).map_err(|e| e.to_string())?;
        s.notebook
            .metadata
            .additional
            .insert("deno".to_string(), deno_value);
        s.dirty = true;
    }
    push_metadata_to_sync(&state, &notebook_sync).await;
    Ok(())
}

/// Format a cell's source code using the appropriate formatter (ruff for Python, deno fmt for TypeScript/JavaScript).
/// Returns the formatted source and whether it changed. If formatting fails (e.g., syntax error),
/// returns the original source with an error message.
#[tauri::command]
async fn format_cell(
    cell_id: String,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<format::FormatResult, String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    // Get current source and runtime
    let (source, runtime) = {
        let nb = state.lock().map_err(|e| e.to_string())?;
        let src = nb
            .get_cell_source(&cell_id)
            .ok_or_else(|| "Cell not found".to_string())?;
        let rt = nb.get_runtime();
        (src, rt)
    };

    // Skip formatting for empty cells
    if source.trim().is_empty() {
        return Ok(format::FormatResult {
            source,
            changed: false,
            error: None,
        });
    }

    // Format based on runtime
    let mut result = match runtime {
        Runtime::Python => format::format_python(&source)
            .await
            .map_err(|e| e.to_string())?,
        Runtime::Deno => format::format_deno(&source, "typescript")
            .await
            .map_err(|e| e.to_string())?,
        Runtime::Other(ref s) => {
            return Err(format!("No formatter available for runtime: {s}"));
        }
    };

    // Strip trailing newline that formatters always add (cells shouldn't end with \n)
    result.source = result.source_for_cell().to_string();
    result.changed = result.source != source;

    // If formatting changed the source, update the backend state and notify frontend
    if result.changed {
        {
            let mut nb = state.lock().map_err(|e| e.to_string())?;
            nb.update_cell_source(&cell_id, &result.source);
        }
        // Emit event to notify frontend of the source change
        let _ = emit_to_label::<_, _, _>(
            &window,
            window.label(),
            "cell:source_updated",
            serde_json::json!({
                "cell_id": cell_id,
                "source": result.source.clone(),
            }),
        );
    }

    Ok(result)
}

/// Check if a formatter is available for the current notebook runtime.
/// Returns true if ruff is available for Python notebooks or deno for TypeScript notebooks.
#[tauri::command]
async fn check_formatter_available(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<bool, String> {
    let state = notebook_state_for_window(&window, registry.inner())?;
    let runtime = {
        let nb = state.lock().map_err(|e| e.to_string())?;
        nb.get_runtime()
    };

    match runtime {
        Runtime::Python => Ok(format::check_ruff_available().await),
        Runtime::Deno => Ok(deno_env::check_deno_available().await),
        Runtime::Other(_) => Ok(false),
    }
}

/// Get app settings (default runtime, etc.)
#[tauri::command]
async fn get_settings() -> runtimed::settings_doc::SyncedSettings {
    settings::load_settings()
}

/// Set the default runtime preference
#[tauri::command]
async fn set_default_runtime(runtime: Runtime) -> Result<(), String> {
    let mut settings = settings::load_settings();
    settings.default_runtime = runtime;
    settings::save_settings(&settings).map_err(|e| e.to_string())
}

/// Set the default Python environment type (uv or conda)
#[tauri::command]
async fn set_default_python_env(env_type: String) -> Result<(), String> {
    let python_env: settings::PythonEnvType = env_type
        .parse()
        .expect("FromStr for PythonEnvType is infallible");

    let mut settings = settings::load_settings();
    settings.default_python_env = python_env;
    settings::save_settings(&settings).map_err(|e| e.to_string())
}

/// Get synced settings from the Automerge settings document via runtimed.
/// Falls back to reading settings.json when the daemon is unavailable,
/// so the frontend always gets real settings instead of hardcoded defaults.
#[tauri::command]
async fn get_synced_settings() -> Result<runtimed::settings_doc::SyncedSettings, String> {
    match runtimed::sync_client::try_get_synced_settings().await {
        Ok(settings) => {
            log::info!(
                "[settings] get_synced_settings from daemon: runtime={}, env={}",
                settings.default_runtime,
                settings.default_python_env
            );
            Ok(settings)
        }
        Err(e) => {
            log::warn!(
                "[settings] Daemon unavailable ({}), falling back to settings.json",
                e
            );
            let settings = settings::load_settings();
            log::info!(
                "[settings] get_synced_settings from JSON fallback: runtime={}, env={}",
                settings.default_runtime,
                settings.default_python_env
            );
            Ok(settings)
        }
    }
}

/// Persist a setting to local settings.json (for keys that have local representation).
fn save_setting_locally(key: &str, value: &serde_json::Value) -> Result<(), String> {
    match key {
        "theme" => {
            let value_str = value.as_str().ok_or("expected string")?;
            let theme: runtimed::settings_doc::ThemeMode =
                serde_json::from_str(&format!("\"{value_str}\"")).map_err(|e| e.to_string())?;
            let mut s = settings::load_settings();
            s.theme = theme;
            settings::save_settings(&s).map_err(|e| e.to_string())
        }
        "default_runtime" => {
            let value_str = value.as_str().ok_or("expected string")?;
            let runtime: Runtime =
                serde_json::from_str(&format!("\"{}\"", value_str)).map_err(|e| e.to_string())?;
            let mut s = settings::load_settings();
            s.default_runtime = runtime;
            settings::save_settings(&s).map_err(|e| e.to_string())
        }
        "default_python_env" => {
            let value_str = value.as_str().ok_or("expected string")?;
            let env_type: settings::PythonEnvType = value_str
                .parse()
                .expect("FromStr for PythonEnvType is infallible");
            let mut s = settings::load_settings();
            s.default_python_env = env_type;
            settings::save_settings(&s).map_err(|e| e.to_string())
        }
        "uv.default_packages" => {
            let packages = json_value_to_string_vec(value);
            let mut s = settings::load_settings();
            s.uv.default_packages = packages;
            settings::save_settings(&s).map_err(|e| e.to_string())
        }
        "conda.default_packages" => {
            let packages = json_value_to_string_vec(value);
            let mut s = settings::load_settings();
            s.conda.default_packages = packages;
            settings::save_settings(&s).map_err(|e| e.to_string())
        }
        _ => Ok(()),
    }
}

/// Extract a Vec<String> from a JSON value (array of strings).
fn json_value_to_string_vec(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        serde_json::Value::String(s) => runtimed::settings_doc::split_comma_list(s),
        _ => vec![],
    }
}

/// Update a synced setting via the daemon and persist locally.
#[tauri::command]
async fn set_synced_setting(key: String, value: serde_json::Value) -> Result<(), String> {
    // Always persist to local settings.json so the menu handler can read it synchronously
    save_setting_locally(&key, &value)?;

    // Best-effort sync via daemon — use a short timeout since local write already succeeded
    #[cfg(unix)]
    {
        let socket_path = runtimed::default_socket_path();
        match runtimed::sync_client::SyncClient::connect_with_timeout(
            socket_path,
            std::time::Duration::from_millis(500),
        )
        .await
        {
            Ok(mut client) => {
                client
                    .put_value(&key, &value)
                    .await
                    .map_err(|e| format!("sync error: {}", e))?;
            }
            Err(e) => {
                log::warn!("[settings] Sync daemon unavailable ({}), local-only", e);
            }
        }
    }

    #[cfg(windows)]
    {
        let socket_path = runtimed::default_socket_path();
        match runtimed::sync_client::SyncClient::connect_with_timeout(
            socket_path,
            std::time::Duration::from_millis(500),
        )
        .await
        {
            Ok(mut client) => {
                client
                    .put_value(&key, &value)
                    .await
                    .map_err(|e| format!("sync error: {}", e))?;
            }
            Err(e) => {
                log::warn!("[settings] Sync daemon unavailable ({}), local-only", e);
            }
        }
    }

    Ok(())
}

fn focused_window(app: &tauri::AppHandle) -> Option<tauri::WebviewWindow> {
    app.webview_windows()
        .into_values()
        .find(|window| window.is_focused().ok() == Some(true))
}

/// Create a new notebook window with the specified runtime.
fn spawn_new_notebook(
    app: &tauri::AppHandle,
    registry: &WindowNotebookRegistry,
    runtime: Runtime,
) -> Result<(), String> {
    let state = NotebookState::new_empty_with_runtime(runtime);
    create_notebook_window(app, registry, state).map(|_| ())
}

/// Ensure ~/notebooks directory exists and return its path.
fn ensure_notebooks_directory() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    let notebooks_dir = home.join("notebooks");
    match std::fs::create_dir(&notebooks_dir) {
        Ok(()) => Ok(notebooks_dir),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Only return the path if it's actually a directory
            if notebooks_dir.is_dir() {
                Ok(notebooks_dir)
            } else {
                Err(format!(
                    "~/notebooks exists but is not a directory: {}",
                    notebooks_dir.display()
                ))
            }
        }
        Err(e) => Err(format!("Failed to create ~/notebooks directory: {}", e)),
    }
}

/// Get the default directory for saving new notebooks.
#[tauri::command]
async fn get_default_save_directory() -> Result<String, String> {
    ensure_notebooks_directory().map(|p| p.to_string_lossy().to_string())
}

/// Background task that subscribes to settings changes from the runtimed daemon
/// and emits Tauri events to all windows when settings change.
///
/// Reconnects automatically with backoff if the connection drops.
#[cfg(unix)]
async fn run_settings_sync(app: tauri::AppHandle) {
    use tauri::Emitter;

    let socket_path = runtimed::default_socket_path();

    loop {
        match runtimed::sync_client::SyncClient::connect(socket_path.clone()).await {
            Ok(mut client) => {
                // Emit initial settings
                let settings = client.get_all();
                log::info!(
                    "[settings-sync] Initial emit: runtime={}, env={}",
                    settings.default_runtime,
                    settings.default_python_env
                );
                let _ = app.emit("settings:changed", &settings);

                // Watch for changes
                loop {
                    match client.recv_changes().await {
                        Ok(settings) => {
                            log::info!("[settings-sync] Settings changed: {:?}", settings);
                            let _ = app.emit("settings:changed", &settings);
                        }
                        Err(e) => {
                            log::warn!("[settings-sync] Disconnected: {}", e);
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                log::info!(
                    "[settings-sync] Cannot connect to sync daemon: {}. Retrying in 5s.",
                    e
                );
            }
        }

        // Backoff before reconnecting
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

#[cfg(windows)]
async fn run_settings_sync(app: tauri::AppHandle) {
    use tauri::Emitter;

    let socket_path = runtimed::default_socket_path();

    loop {
        match runtimed::sync_client::SyncClient::connect(socket_path.clone()).await {
            Ok(mut client) => {
                // Emit initial settings
                let settings = client.get_all();
                let _ = app.emit("settings:changed", &settings);

                // Watch for changes
                loop {
                    match client.recv_changes().await {
                        Ok(settings) => {
                            log::info!("[settings-sync] Settings changed: {:?}", settings);
                            let _ = app.emit("settings:changed", &settings);
                        }
                        Err(e) => {
                            log::warn!("[settings-sync] Disconnected: {}", e);
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                log::info!(
                    "[settings-sync] Cannot connect to sync daemon: {}. Retrying in 5s.",
                    e
                );
            }
        }

        // Backoff before reconnecting
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

/// Create initial notebook state for a new notebook, detecting project-level config for Python.
fn create_new_notebook_state(path: &Path, runtime: Runtime) -> NotebookState {
    // Only check project files for Python runtime
    if runtime == Runtime::Python {
        // Check pyproject.toml first (uv)
        if let Some(pyproject_path) = pyproject::find_pyproject(path) {
            if let Ok(config) = pyproject::parse_pyproject(&pyproject_path) {
                info!(
                    "New notebook at {}: detected pyproject.toml at {}, using UV",
                    path.display(),
                    pyproject_path.display()
                );
                let mut state = NotebookState::new_empty_with_uv_from_pyproject(&config);
                state.path = Some(path.to_path_buf());
                return state;
            }
        }

        // Check environment.yml (conda)
        if let Some(yml_path) = environment_yml::find_environment_yml(path) {
            if let Ok(config) = environment_yml::parse_environment_yml(&yml_path) {
                if !config.dependencies.is_empty() {
                    info!(
                        "New notebook at {}: detected environment.yml at {}, using conda",
                        path.display(),
                        yml_path.display()
                    );
                    let mut state =
                        NotebookState::new_empty_with_conda_from_environment_yml(&config);
                    state.path = Some(path.to_path_buf());
                    return state;
                }
            }
        }
    }

    // No project-level config found (or non-Python runtime) - use default
    let mut state = NotebookState::new_empty_with_runtime(runtime);
    state.path = Some(path.to_path_buf());
    state
}

fn load_notebook_state_for_path(path: &Path, runtime: Runtime) -> Result<NotebookState, String> {
    if path.exists() {
        let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let nb = nbformat::parse_notebook(&content).map_err(|e| e.to_string())?;
        let mut nb_v4 = match nb {
            nbformat::Notebook::V4(nb) => nb,
            nbformat::Notebook::Legacy(legacy) => {
                nbformat::upgrade_legacy_notebook(legacy).map_err(|e| e.to_string())?
            }
            nbformat::Notebook::V3(v3) => {
                nbformat::upgrade_v3_notebook(v3).map_err(|e| e.to_string())?
            }
        };
        notebook_state::migrate_legacy_metadata(&mut nb_v4.metadata.additional);
        Ok(NotebookState::from_notebook(nb_v4, path.to_path_buf()))
    } else {
        Ok(create_new_notebook_state(path, runtime))
    }
}

fn create_window_context(state: NotebookState) -> WindowNotebookContext {
    WindowNotebookContext {
        notebook_state: Arc::new(Mutex::new(state)),
        notebook_sync: Arc::new(tokio::sync::Mutex::new(None)),
    }
}

/// Run the notebook Tauri app.
///
/// If `notebook_path` is Some, opens that file. If None, creates a new empty notebook.
/// The `runtime` parameter specifies which runtime to use for new notebooks.
/// If None, falls back to user's default runtime from settings.
pub fn run(
    notebook_path: Option<PathBuf>,
    runtime: Option<Runtime>,
    #[allow(unused_variables)] webdriver_port: Option<u16>,
) -> anyhow::Result<()> {
    env_logger::init();
    shell_env::load_shell_environment();

    // Use provided runtime or fall back to user's default from settings
    let runtime = runtime.unwrap_or_else(|| settings::load_settings().default_runtime);

    let initial_state = match notebook_path.as_ref() {
        Some(path) => load_notebook_state_for_path(path, runtime).map_err(anyhow::Error::msg)?,
        None => NotebookState::new_empty_with_runtime(runtime),
    };

    let window_title = notebook_path
        .as_ref()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("Untitled.ipynb")
        .to_string();

    let window_registry = WindowNotebookRegistry::default();
    let main_context = create_window_context(initial_state);
    window_registry
        .insert("main", main_context.clone())
        .map_err(anyhow::Error::msg)?;

    // Create the prewarming environment pools (UV and Conda)
    let env_pool: env_pool::SharedEnvPool = Arc::new(tokio::sync::Mutex::new(
        env_pool::EnvPool::new(env_pool::PoolConfig::default()),
    ));
    let conda_env_pool: env_pool::SharedCondaEnvPool = Arc::new(tokio::sync::Mutex::new(
        env_pool::CondaEnvPool::new(env_pool::PoolConfig::default()),
    ));

    // Track auto-launch state for frontend to query
    let auto_launch_in_progress = AutoLaunchInProgress(Arc::new(AtomicBool::new(false)));

    // Guard against concurrent reconnect attempts
    let reconnect_in_progress = ReconnectInProgress(Arc::new(AtomicBool::new(false)));

    // Recovery completion flags - set when prewarming loops finish recovery
    let uv_recovery_complete = Arc::new(AtomicBool::new(false));
    let conda_recovery_complete = Arc::new(AtomicBool::new(false));

    // Daemon sync completion flag - set when notebook sync initialization completes
    // Used to coordinate auto-launch decision with daemon connection status
    let daemon_sync_complete = Arc::new(AtomicBool::new(false));
    let daemon_sync_success = Arc::new(AtomicBool::new(false));

    // Clone for setup closure
    let pool_for_prewarm = env_pool.clone();
    let conda_pool_for_prewarm = conda_env_pool.clone();
    let uv_recovery_for_prewarm = uv_recovery_complete.clone();
    let conda_recovery_for_prewarm = conda_recovery_complete.clone();

    // Clone for notebook sync initialization
    let registry_for_sync = window_registry.clone();
    let daemon_sync_complete_for_init = daemon_sync_complete.clone();
    let daemon_sync_success_for_init = daemon_sync_success.clone();

    // Clone for auto-launch coordination
    let daemon_sync_complete_for_autolaunch = daemon_sync_complete.clone();
    let daemon_sync_success_for_autolaunch = daemon_sync_success.clone();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(window_registry.clone())
        .manage(env_pool)
        .manage(conda_env_pool)
        .manage(auto_launch_in_progress)
        .manage(reconnect_in_progress)
        .invoke_handler(tauri::generate_handler![
            // Notebook file operations
            load_notebook,
            has_notebook_path,
            get_notebook_path,
            save_notebook,
            save_notebook_as,
            get_default_save_directory,
            clone_notebook_to_path,
            open_notebook_in_new_window,
            // Cell operations
            update_cell_source,
            add_cell,
            delete_cell,
            // Daemon kernel operations (all kernel ops go through daemon)
            launch_kernel_via_daemon,
            queue_cell_via_daemon,
            execute_cell_via_daemon,
            clear_outputs_via_daemon,
            interrupt_via_daemon,
            shutdown_kernel_via_daemon,
            get_daemon_kernel_info,
            is_daemon_connected,
            get_daemon_queue_state,
            run_all_cells_via_daemon,
            send_comm_via_daemon,
            get_history_via_daemon,
            complete_via_daemon,
            reconnect_to_daemon,
            refresh_from_automerge,
            debug_get_automerge_state,
            debug_get_local_state,
            // Kernelspec discovery (used by UI)
            get_preferred_kernelspec,
            list_kernelspecs,
            // UV dependency management
            check_uv_available,
            get_notebook_dependencies,
            set_notebook_dependencies,
            add_dependency,
            remove_dependency,
            clear_dependency_section,
            // Conda dependency management
            get_conda_dependencies,
            set_conda_dependencies,
            add_conda_dependency,
            remove_conda_dependency,
            // pyproject.toml discovery
            detect_pyproject,
            get_pyproject_dependencies,
            import_pyproject_dependencies,
            // pixi.toml support
            detect_pixi_toml,
            get_pixi_dependencies,
            import_pixi_dependencies,
            // environment.yml support
            detect_environment_yml,
            get_environment_yml_dependencies,
            // Trust verification
            verify_notebook_trust,
            approve_notebook_trust,
            check_typosquats,
            // Deno kernel support
            check_deno_available,
            get_deno_version,
            get_notebook_runtime,
            detect_deno_config,
            get_deno_permissions,
            set_deno_permissions,
            get_deno_flexible_npm_imports,
            set_deno_flexible_npm_imports,
            // Code formatting
            format_cell,
            check_formatter_available,
            // Settings
            get_settings,
            set_default_runtime,
            set_default_python_env,
            // Synced settings (via runtimed Automerge)
            get_synced_settings,
            set_synced_setting,
            // Debug info
            get_git_info,
            get_prewarm_status,
            get_conda_pool_status,
            get_daemon_info,
            get_blob_port,
        ])
        .setup(move |app| {
            let setup_start = std::time::Instant::now();
            log::info!("[startup] App setup starting");

            // Ensure ~/notebooks directory exists for new notebook saves and kernel CWD
            let notebooks_dir = ensure_notebooks_directory()
                .map_err(Box::<dyn std::error::Error>::from)?;
            log::info!("[startup] Notebooks directory: {}", notebooks_dir.display());

            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_title(&window_title);
            }

            // Start WebDriver server for native E2E testing (if enabled)
            #[cfg(feature = "webdriver-test")]
            if let Some(port) = webdriver_port {
                log::info!("[webdriver] Starting built-in WebDriver server on port {}", port);
                webdriver::start_server(app.handle().clone(), port);

                // Prevent the app from stealing focus during E2E tests.
                // NSApplicationActivationPolicyAccessory keeps the window visible
                // and functional but won't activate (steal focus) or show in the Dock.
                #[cfg(target_os = "macos")]
                unsafe {
                    use cocoa::appkit::{NSApplication, NSApplicationActivationPolicy};
                    use cocoa::base::nil;
                    let ns_app = NSApplication::sharedApplication(nil);
                    ns_app.setActivationPolicy_(NSApplicationActivationPolicy::NSApplicationActivationPolicyAccessory);
                }
            }

            // Set up native menu bar
            let menu = crate::menu::create_menu(app.handle())?;
            app.set_menu(menu)?;

            // Ensure runtimed is running (required for daemon-only mode)
            // The daemon provides centralized prewarming across all notebook windows
            let app_for_daemon = app.handle().clone();
            let app_for_sync = app.handle().clone();
            let app_for_notebook_sync = app.handle().clone();
            let registry_for_notebook_sync = registry_for_sync.clone();
            tauri::async_runtime::spawn(async move {
                // Get path to bundled runtimed binary (for auto-installation)
                let binary_path = get_bundled_runtimed_path(&app_for_daemon);

                // Create progress callback to emit Tauri events for UI feedback
                let app_for_progress = app_for_daemon.clone();
                let on_progress = move |progress: runtimed::client::DaemonProgress| {
                    let _ = app_for_progress.emit("daemon:progress", &progress);
                };

                let daemon_available =
                    match runtimed::client::ensure_daemon_running(binary_path, Some(on_progress))
                        .await
                    {
                        Ok(endpoint) => {
                            log::info!("[startup] runtimed running at {}", endpoint);
                            true
                        }
                        Err(e) => {
                            // Not critical - in-process prewarming will work as fallback
                            log::info!(
                                "[startup] runtimed not available: {}. Using in-process prewarming.",
                                e
                            );
                            false
                        }
                    };

                // Start settings sync subscription (reconnects automatically)
                // Spawn as separate task since it runs forever
                tokio::spawn(run_settings_sync(app_for_sync));

                // Initialize notebook sync if daemon is available
                if daemon_available {
                    match (
                        app_for_notebook_sync.get_webview_window("main"),
                        registry_for_notebook_sync.get("main"),
                    ) {
                        (Some(window), Ok(context)) => {
                            match initialize_notebook_sync(
                                window,
                                context.notebook_state,
                                context.notebook_sync,
                            )
                            .await
                            {
                                Ok(()) => {
                                    log::info!(
                                        "[startup] Notebook sync initialized successfully"
                                    );
                                    daemon_sync_success_for_init
                                        .store(true, Ordering::SeqCst);
                                }
                                Err(e) => {
                                    log::warn!(
                                        "[startup] Notebook sync initialization failed: {}",
                                        e
                                    );
                                }
                            }
                        }
                        (None, _) => {
                            log::warn!("[startup] Main window missing during sync init");
                        }
                        (_, Err(e)) => {
                            log::warn!("[startup] Main notebook context missing: {}", e);
                        }
                    }
                }
                // Signal that daemon sync attempt is complete (success or failure)
                daemon_sync_complete_for_init.store(true, Ordering::SeqCst);
            });

            // Spawn the UV environment prewarming loop
            let app_for_prewarm = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                env_pool::run_prewarming_loop(pool_for_prewarm, app_for_prewarm, uv_recovery_for_prewarm).await;
            });

            // Spawn the conda environment prewarming loop
            tauri::async_runtime::spawn(async move {
                env_pool::run_conda_prewarming_loop(conda_pool_for_prewarm, conda_recovery_for_prewarm).await;
            });

            // Wait for daemon sync to complete before considering startup done
            log::info!("[startup] Setup complete in {}ms, spawning daemon sync wait task", setup_start.elapsed().as_millis());
            let app_for_autolaunch = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let autolaunch_start = std::time::Instant::now();

                log::info!("[autolaunch] Waiting for daemon sync...");

                // Wait up to 10 seconds for daemon sync to complete
                // This needs to be long enough for large notebooks with many cells
                let sync_timeout = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    async {
                        while !daemon_sync_complete_for_autolaunch.load(Ordering::SeqCst) {
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        }
                    },
                )
                .await;

                let sync_wait_ms = autolaunch_start.elapsed().as_millis();

                if sync_timeout.is_err() {
                    // Daemon sync timed out - emit error event for frontend to display
                    log::error!(
                        "[autolaunch] Daemon sync timed out after {}ms. Daemon is not available.",
                        sync_wait_ms
                    );
                    let _ = app_for_autolaunch.emit("daemon:unavailable", serde_json::json!({
                        "reason": "sync_timeout",
                        "message": "Daemon sync timed out. The runtime daemon may not be running.",
                        "guidance": "Run 'cargo xtask dev-daemon' in another terminal (dev mode), or check daemon status with 'runt daemon status'."
                    }));
                } else if daemon_sync_success_for_autolaunch.load(Ordering::SeqCst) {
                    // Daemon sync succeeded - daemon handles auto-launch
                    log::info!(
                        "[autolaunch] Daemon sync succeeded in {}ms, daemon handles auto-launch",
                        sync_wait_ms
                    );
                } else {
                    // Daemon sync completed but failed - emit error event
                    log::error!(
                        "[autolaunch] Daemon sync failed after {}ms. Connection failed.",
                        sync_wait_ms
                    );
                    let _ = app_for_autolaunch.emit("daemon:unavailable", serde_json::json!({
                        "reason": "sync_failed",
                        "message": "Failed to connect to runtime daemon.",
                        "guidance": "Run 'cargo xtask dev-daemon' in another terminal (dev mode), or check daemon status with 'runt daemon status'."
                    }));
                }
            });

            Ok(())
        })
        .on_menu_event(|app, event| {
            let menu_id = event.id().as_ref();
            let registry = app.state::<WindowNotebookRegistry>();
            match menu_id {
                crate::menu::MENU_NEW_NOTEBOOK => {
                    // Spawn notebook using the user's default runtime preference
                    let runtime = settings::load_settings().default_runtime;
                    let _ = spawn_new_notebook(app, registry.inner(), runtime);
                }
                crate::menu::MENU_NEW_PYTHON_NOTEBOOK => {
                    let _ = spawn_new_notebook(app, registry.inner(), Runtime::Python);
                }
                crate::menu::MENU_NEW_DENO_NOTEBOOK => {
                    let _ = spawn_new_notebook(app, registry.inner(), Runtime::Deno);
                }
                crate::menu::MENU_OPEN => {
                    // Emit event to frontend to trigger open dialog
                    if let Some(window) = focused_window(app) {
                        let _ = emit_to_label::<_, _, _>(&window, window.label(), "menu:open", ());
                    }
                }
                crate::menu::MENU_SAVE => {
                    // Emit event to frontend to trigger save
                    if let Some(window) = focused_window(app) {
                        let _ = emit_to_label::<_, _, _>(&window, window.label(), "menu:save", ());
                    }
                }
                crate::menu::MENU_CLONE_NOTEBOOK => {
                    // Emit event to frontend to trigger clone
                    if let Some(window) = focused_window(app) {
                        let _ = emit_to_label::<_, _, _>(&window, window.label(), "menu:clone", ());
                    }
                }
                crate::menu::MENU_ZOOM_IN => {
                    if let Some(window) = focused_window(app) {
                        let _ = emit_to_label::<_, _, _>(&window, window.label(), "menu:zoom-in", ());
                    }
                }
                crate::menu::MENU_ZOOM_OUT => {
                    if let Some(window) = focused_window(app) {
                        let _ = emit_to_label::<_, _, _>(&window, window.label(), "menu:zoom-out", ());
                    }
                }
                crate::menu::MENU_ZOOM_RESET => {
                    if let Some(window) = focused_window(app) {
                        let _ = emit_to_label::<_, _, _>(&window, window.label(), "menu:zoom-reset", ());
                    }
                }
                crate::menu::MENU_RUN_ALL_CELLS => {
                    if let Some(window) = focused_window(app) {
                        let _ = emit_to_label::<_, _, _>(&window, window.label(), "menu:run-all", ());
                    }
                }
                crate::menu::MENU_RESTART_AND_RUN_ALL => {
                    if let Some(window) = focused_window(app) {
                        let _ = emit_to_label::<_, _, _>(
                            &window,
                            window.label(),
                            "menu:restart-and-run-all",
                            (),
                        );
                    }
                }
                crate::menu::MENU_INSTALL_CLI => {
                    let app_handle = app.clone();
                    match crate::cli_install::install_cli(&app_handle) {
                        Ok(()) => {
                            log::info!("[cli_install] CLI installed successfully");
                            // Show success dialog
                            tauri::async_runtime::spawn(async move {
                                let _ = tauri_plugin_dialog::DialogExt::dialog(&app_handle)
                                    .message("The 'runt' and 'nb' commands have been installed to /usr/local/bin.\n\nYou can now use:\n  runt notebook    - Open notebook app\n  nb               - Shorthand for above\n  runt ps          - List running kernels")
                                    .title("CLI Installed")
                                    .kind(tauri_plugin_dialog::MessageDialogKind::Info)
                                    .blocking_show();
                            });
                        }
                        Err(e) => {
                            log::error!("[cli_install] CLI installation failed: {}", e);
                            // Show error dialog
                            tauri::async_runtime::spawn(async move {
                                let _ = tauri_plugin_dialog::DialogExt::dialog(&app_handle)
                                    .message(format!("Failed to install CLI: {}", e))
                                    .title("Installation Failed")
                                    .kind(tauri_plugin_dialog::MessageDialogKind::Error)
                                    .blocking_show();
                            });
                        }
                    }
                }
                _ => {
                    if let Some(sample) = crate::menu::sample_for_menu_item_id(menu_id) {
                        if let Err(e) = open_bundled_sample_notebook(app, registry.inner(), sample) {
                            log::error!(
                                "[sample_notebooks] Failed to open sample {}: {}",
                                sample.id,
                                e
                            );
                            let app_handle = app.clone();
                            tauri::async_runtime::spawn(async move {
                                let _ = tauri_plugin_dialog::DialogExt::dialog(&app_handle)
                                    .message(format!(
                                        "Failed to open sample notebook '{}': {}",
                                        sample.title, e
                                    ))
                                    .title("Sample Notebook Error")
                                    .kind(tauri_plugin_dialog::MessageDialogKind::Error)
                                    .blocking_show();
                            });
                        }
                    }
                }
            }
        })
        .build(tauri::generate_context!())
        .map_err(|e| anyhow::anyhow!("Tauri build error: {}", e))?;

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let registry_for_open = window_registry.clone();
    app.run(move |_app_handle, _event| {
        // Handle file associations (macOS only)
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        if let RunEvent::Opened { urls } = &_event {
            for url in urls {
                let path = match url.scheme() {
                    "file" => url.to_file_path().ok(),
                    _ => None,
                };
                let Some(path) = path else { continue };
                if path.extension().and_then(|e| e.to_str()) != Some("ipynb") {
                    continue;
                }

                let main_is_empty = registry_for_open
                    .get("main")
                    .ok()
                    .and_then(|context| {
                        context
                            .notebook_state
                            .lock()
                            .ok()
                            .map(|state| state.path.is_none() && !state.dirty)
                    })
                    .unwrap_or(false);

                if main_is_empty {
                    match load_notebook_state_for_path(
                        &path,
                        settings::load_settings().default_runtime,
                    ) {
                        Ok(new_state) => {
                            if let Ok(context) = registry_for_open.get("main") {
                                if let Ok(mut state) = context.notebook_state.lock() {
                                    *state = new_state;
                                }
                            }

                            if let Some(window) = _app_handle.get_webview_window("main") {
                                let title = path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("Untitled.ipynb");
                                let _ = window.set_title(title);
                                let _ = emit_to_label::<_, _, _>(
                                    &window,
                                    window.label(),
                                    "notebook:file-opened",
                                    (),
                                );
                            }
                        }
                        Err(e) => log::error!("Failed to load notebook file: {}", e),
                    }
                } else if let Err(e) = open_notebook_window(_app_handle, &registry_for_open, &path)
                {
                    log::error!("Failed to open notebook in new window: {}", e);
                }
            }
        }
    });

    Ok(())
}
