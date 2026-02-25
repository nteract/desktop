pub mod cli_install;
pub mod conda_env;
pub mod deno_env;
pub mod env_pool;
pub mod environment_yml;
pub mod execution_queue;
pub mod format;
pub mod kernel;
pub mod menu;
pub mod notebook_state;
pub mod pixi;
pub mod project_file;
pub mod pyproject;
pub mod runtime;
pub mod settings;
pub mod shell_env;
pub mod tools;
pub mod trust;
pub mod typosquat;
pub mod uv_env;
#[cfg(feature = "webdriver-test")]
pub mod webdriver;

pub use runtime::Runtime;

use execution_queue::{ExecutionQueue, ExecutionQueueState, QueueCommand, SharedExecutionQueue};
use kernel::{CompletionResult, HistoryResult, NotebookKernel};
use notebook_state::{FrontendCell, NotebookState};
use runtimed::notebook_doc::CellSnapshot;
use runtimed::notebook_sync_client::{NotebookSyncClient, NotebookSyncHandle};
use runtimed::protocol::{NotebookRequest, NotebookResponse};

use log::{error, info, warn};
use nbformat::v4::{Cell, CellId, CellMetadata};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Shared notebook sync handle for cross-window state synchronization.
/// The Option allows graceful fallback when daemon is unavailable.
/// Uses the split handle pattern - the handle is clonable and doesn't block.
type SharedNotebookSync = Arc<tokio::sync::Mutex<Option<NotebookSyncHandle>>>;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager, RunEvent};
use tokio::sync::mpsc;

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

/// Kernel lifecycle event for frontend status updates.
#[derive(Debug, Clone, Serialize)]
struct KernelLifecycleEvent {
    state: String,
    runtime: String,
    /// Environment source identifier, present when state is "ready".
    /// Values: "uv:inline", "uv:pyproject", "uv:prewarmed", "uv:fresh",
    ///         "conda:inline", "conda:pixi", "conda:prewarmed", "conda:fresh"
    #[serde(skip_serializing_if = "Option::is_none")]
    env_source: Option<String>,
    /// Error message, present when state is "error".
    #[serde(skip_serializing_if = "Option::is_none")]
    error_message: Option<String>,
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
    app: tauri::AppHandle,
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

    // Connect using the split pattern - returns handle, receiver, broadcast receiver, and initial cells
    let (handle, mut receiver, mut broadcast_receiver, initial_cells) =
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
        }
        // Emit Automerge state to frontend (for immediate UI update)
        if let Err(e) = app.emit("notebook:updated", &initial_cells) {
            warn!("[notebook-sync] Failed to emit initial cells: {}", e);
        }
    }

    // Store the handle for commands to use
    *notebook_sync.lock().await = Some(handle);

    // Spawn receiver task for cross-window sync
    // The receiver is separate from the handle, so it doesn't block commands
    let app_clone = app.clone();
    tokio::spawn(async move {
        info!("[notebook-sync] Starting receiver loop");
        while let Some(cells) = receiver.recv().await {
            info!("[notebook-sync] Received {} cells from peer", cells.len());
            // Emit event for frontend to reconcile state
            if let Err(e) = app_clone.emit("notebook:updated", &cells) {
                warn!("[notebook-sync] Failed to emit notebook:updated: {}", e);
            }
        }
        info!("[notebook-sync] Receiver loop ended");
    });

    // Spawn broadcast receiver task for daemon kernel events
    let notebook_sync_for_disconnect = notebook_sync.clone();
    tokio::spawn(async move {
        info!("[notebook-sync] Starting broadcast receiver loop");
        while let Some(broadcast) = broadcast_receiver.recv().await {
            info!("[notebook-sync] Received broadcast: {:?}", broadcast);
            // Emit broadcast events to frontend
            if let Err(e) = app.emit("daemon:broadcast", &broadcast) {
                warn!("[notebook-sync] Failed to emit daemon:broadcast: {}", e);
            }
        }
        info!("[notebook-sync] Broadcast receiver loop ended - daemon disconnected");

        // Clear the handle so operations fail gracefully
        *notebook_sync_for_disconnect.lock().await = None;

        // Emit disconnection event so frontend can reset kernel state
        if let Err(e) = app.emit("daemon:disconnected", ()) {
            warn!("[notebook-sync] Failed to emit daemon:disconnected: {}", e);
        }
    });

    info!(
        "[notebook-sync] Initialization complete for {}",
        notebook_id
    );
    Ok(())
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
}

/// Get daemon info for the debug banner.
/// Returns None in release builds or if daemon.json doesn't exist.
#[tauri::command]
async fn get_daemon_info() -> Option<DaemonInfoForBanner> {
    #[cfg(debug_assertions)]
    {
        let info_path = dirs::cache_dir()?.join("runt").join("daemon.json");
        let contents = std::fs::read_to_string(info_path).ok()?;
        let json: serde_json::Value = serde_json::from_str(&contents).ok()?;
        let version = json.get("version")?.as_str()?.to_string();
        Some(DaemonInfoForBanner { version })
    }
    #[cfg(not(debug_assertions))]
    {
        None
    }
}

#[tauri::command]
async fn load_notebook(
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<Vec<FrontendCell>, String> {
    let state = state.lock().map_err(|e| e.to_string())?;
    Ok(state.cells_for_frontend())
}

/// Check if the notebook has a file path set
#[tauri::command]
async fn has_notebook_path(
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<bool, String> {
    let state = state.lock().map_err(|e| e.to_string())?;
    Ok(state.path.is_some())
}

/// Get the current notebook file path
#[tauri::command]
async fn get_notebook_path(
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<Option<String>, String> {
    let state = state.lock().map_err(|e| e.to_string())?;
    Ok(state.path.as_ref().map(|p| p.to_string_lossy().to_string()))
}

/// Format all code cells in the notebook and save.
/// Formatting is best-effort - cells that fail to format are saved as-is.
#[tauri::command]
async fn save_notebook(
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    app: tauri::AppHandle,
) -> Result<(), String> {
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
                let _ = app.emit(
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

    // Now save
    let mut nb = state.lock().map_err(|e| e.to_string())?;
    let content = nb.serialize()?;
    std::fs::write(&path, &content).map_err(|e| e.to_string())?;
    nb.dirty = false;
    Ok(())
}

/// Save notebook to a specific path (Save As).
/// Formats all code cells before saving.
#[tauri::command]
async fn save_notebook_as(
    path: String,
    window: tauri::Window,
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    app: tauri::AppHandle,
) -> Result<(), String> {
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
                let _ = app.emit(
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
/// If the kernel is running with a UV environment, copies it for fast startup.
#[tauri::command]
async fn clone_notebook_to_path(
    path: String,
    notebook_state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
) -> Result<(), String> {
    // Generate fresh env_id upfront
    let new_env_id = uuid::Uuid::new_v4().to_string();

    // Clone notebook structure while holding the lock
    let cloned_notebook = {
        let state = notebook_state.lock().map_err(|e| e.to_string())?;
        let mut cloned = state.notebook.clone();

        // Update runt metadata with new env_id (canonical location for env_id)
        if let Some(runt_value) = cloned.metadata.additional.get_mut("runt") {
            if let Some(obj) = runt_value.as_object_mut() {
                obj.insert("env_id".to_string(), serde_json::json!(new_env_id.clone()));
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

    // If kernel is running with UV env, copy it for fast clone startup
    {
        let kernel = kernel_state.lock().await;
        if let Some(source_env) = kernel.uv_environment() {
            // Copy the environment - ignore errors, clone will just create fresh env on start
            if let Err(e) = uv_env::copy_environment(source_env, &new_env_id).await {
                info!(
                    "Failed to copy environment for clone (will create fresh): {}",
                    e
                );
            }
        }
    }

    // Serialize and write to path
    let nb = nbformat::Notebook::V4(cloned_notebook);
    let content = nbformat::serialize_notebook(&nb).map_err(|e| e.to_string())?;
    std::fs::write(&path, &content).map_err(|e| e.to_string())?;

    Ok(())
}

/// Open a notebook file in a new window (spawns new process)
#[tauri::command]
async fn open_notebook_in_new_window(path: String) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(exe)
        .arg(&path)
        .spawn()
        .map_err(|e| format!("Failed to open notebook: {}", e))?;
    Ok(())
}

#[tauri::command]
async fn update_cell_source(
    cell_id: String,
    source: String,
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    notebook_sync: tauri::State<'_, SharedNotebookSync>,
) -> Result<(), String> {
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
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    notebook_sync: tauri::State<'_, SharedNotebookSync>,
) -> Result<FrontendCell, String> {
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
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    notebook_sync: tauri::State<'_, SharedNotebookSync>,
) -> Result<(), String> {
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

#[tauri::command]
async fn execute_cell(
    cell_id: String,
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
    notebook_sync: tauri::State<'_, SharedNotebookSync>,
) -> Result<String, String> {
    let code = {
        let mut nb = state.lock().map_err(|e| e.to_string())?;
        let src = nb
            .get_cell_source(&cell_id)
            .ok_or_else(|| "Cell not found".to_string())?;
        nb.clear_cell_outputs(&cell_id);
        src
    };

    // Clear outputs in Automerge for cross-window sync
    if let Some(handle) = notebook_sync.lock().await.as_ref() {
        if let Err(e) = handle.clear_outputs(&cell_id).await {
            warn!("[notebook-sync] clear_outputs failed: {}", e);
        }
    }

    info!(
        "execute_cell: cell_id={}, code={:?}",
        cell_id,
        &code[..code.len().min(100)]
    );
    let mut kernel = kernel_state.lock().await;
    let result = kernel
        .execute(&code, &cell_id)
        .await
        .map_err(|e| e.to_string());
    match &result {
        Ok(msg_id) => info!("execute_cell: sent, msg_id={}", msg_id),
        Err(e) => info!("execute_cell: failed: {}", e),
    }
    result
}

/// Sync an output to Automerge for cross-window sync.
/// Called from frontend after receiving iopub output.
#[tauri::command]
async fn sync_append_output(
    cell_id: String,
    output_json: String,
    notebook_sync: tauri::State<'_, SharedNotebookSync>,
) -> Result<(), String> {
    if let Some(handle) = notebook_sync.lock().await.as_ref() {
        if let Err(e) = handle.append_output(&cell_id, &output_json).await {
            warn!("[notebook-sync] append_output failed: {}", e);
        }
    }
    Ok(())
}

/// Sync execution count to Automerge for cross-window sync.
/// Called from frontend after receiving execute_input or execute_result.
#[tauri::command]
async fn sync_execution_count(
    cell_id: String,
    count: i32,
    notebook_sync: tauri::State<'_, SharedNotebookSync>,
) -> Result<(), String> {
    if let Some(handle) = notebook_sync.lock().await.as_ref() {
        if let Err(e) = handle
            .set_execution_count(&cell_id, &count.to_string())
            .await
        {
            warn!("[notebook-sync] set_execution_count failed: {}", e);
        }
    }
    Ok(())
}

// ============================================================================
// Daemon Kernel Operations (Phase 8)
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
    notebook_sync: tauri::State<'_, SharedNotebookSync>,
) -> Result<NotebookResponse, String> {
    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;

    // Use notebook_id from the sync handle if notebook_path not provided
    let resolved_path = notebook_path.or_else(|| Some(handle.notebook_id().to_string()));

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
async fn queue_cell_via_daemon(
    cell_id: String,
    code: String,
    notebook_sync: tauri::State<'_, SharedNotebookSync>,
) -> Result<NotebookResponse, String> {
    info!("[daemon-kernel] queue_cell_via_daemon: cell_id={}", cell_id);

    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;

    handle
        .send_request(NotebookRequest::QueueCell { cell_id, code })
        .await
        .map_err(|e| format!("daemon request failed: {}", e))
}

/// Clear outputs for a cell via the daemon.
#[tauri::command]
async fn clear_outputs_via_daemon(
    cell_id: String,
    notebook_sync: tauri::State<'_, SharedNotebookSync>,
) -> Result<NotebookResponse, String> {
    info!(
        "[daemon-kernel] clear_outputs_via_daemon: cell_id={}",
        cell_id
    );

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
    notebook_sync: tauri::State<'_, SharedNotebookSync>,
) -> Result<NotebookResponse, String> {
    info!("[daemon-kernel] interrupt_via_daemon");

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
    notebook_sync: tauri::State<'_, SharedNotebookSync>,
) -> Result<NotebookResponse, String> {
    info!("[daemon-kernel] shutdown_kernel_via_daemon");

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
    notebook_sync: tauri::State<'_, SharedNotebookSync>,
) -> Result<NotebookResponse, String> {
    info!("[daemon-kernel] get_daemon_kernel_info");

    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;

    handle
        .send_request(NotebookRequest::GetKernelInfo {})
        .await
        .map_err(|e| format!("daemon request failed: {}", e))
}

/// Get execution queue state from the daemon.
#[tauri::command]
async fn get_daemon_queue_state(
    notebook_sync: tauri::State<'_, SharedNotebookSync>,
) -> Result<NotebookResponse, String> {
    info!("[daemon-kernel] get_daemon_queue_state");

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
    notebook_sync: tauri::State<'_, SharedNotebookSync>,
) -> Result<NotebookResponse, String> {
    info!("[daemon-kernel] run_all_cells_via_daemon");

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
    notebook_sync: tauri::State<'_, SharedNotebookSync>,
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

    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;

    handle
        .send_request(NotebookRequest::SendComm { message })
        .await
        .map_err(|e| format!("daemon request failed: {}", e))
}

/// Reconnect to the daemon after a disconnection.
///
/// Called by the frontend after receiving daemon:disconnected event.
#[tauri::command]
async fn reconnect_to_daemon(
    app: tauri::AppHandle,
    notebook_state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    notebook_sync: tauri::State<'_, SharedNotebookSync>,
) -> Result<(), String> {
    info!("[daemon-kernel] reconnect_to_daemon");

    // Check if already connected
    {
        let guard = notebook_sync.lock().await;
        if guard.is_some() {
            info!("[daemon-kernel] Already connected to daemon");
            return Ok(());
        }
    }

    // Re-initialize notebook sync
    initialize_notebook_sync(
        app,
        notebook_state.inner().clone(),
        notebook_sync.inner().clone(),
    )
    .await
}

/// Debug: Get Automerge document state from the daemon.
///
/// Returns the cells as the daemon sees them, useful for debugging sync issues.
#[tauri::command]
async fn debug_get_automerge_state(
    notebook_sync: tauri::State<'_, SharedNotebookSync>,
) -> Result<Vec<serde_json::Value>, String> {
    info!("[debug] Getting Automerge state from daemon");

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
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<Vec<serde_json::Value>, String> {
    info!("[debug] Getting local notebook state");

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

/// Queue a cell for execution. The queue processor will execute cells in FIFO order.
#[tauri::command]
async fn queue_execute_cell(
    cell_id: String,
    queue_tx: tauri::State<'_, mpsc::Sender<QueueCommand>>,
) -> Result<(), String> {
    info!("queue_execute_cell: {}", cell_id);
    queue_tx
        .send(QueueCommand::Enqueue { cell_id })
        .await
        .map_err(|e| e.to_string())
}

/// Clear all pending cells from the execution queue (keeps currently executing cell)
#[tauri::command]
async fn clear_execution_queue(
    queue_tx: tauri::State<'_, mpsc::Sender<QueueCommand>>,
) -> Result<(), String> {
    info!("clear_execution_queue");
    queue_tx
        .send(QueueCommand::Clear)
        .await
        .map_err(|e| e.to_string())
}

/// Get the current execution queue state
#[tauri::command]
async fn get_execution_queue_state(
    queue: tauri::State<'_, SharedExecutionQueue>,
) -> Result<ExecutionQueueState, String> {
    let q = queue.lock().map_err(|e| e.to_string())?;
    Ok(q.get_state())
}

/// Queue all code cells for execution in notebook order.
/// Returns the list of cell IDs that were queued.
#[tauri::command]
async fn run_all_cells(
    app: tauri::AppHandle,
    notebook_state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    queue_tx: tauri::State<'_, mpsc::Sender<QueueCommand>>,
) -> Result<Vec<String>, String> {
    let cell_ids = {
        let mut nb = notebook_state.lock().map_err(|e| e.to_string())?;
        let ids = nb.get_code_cell_ids();
        for id in &ids {
            nb.clear_cell_outputs(id);
        }
        ids
    };
    info!("run_all_cells: {} cells", cell_ids.len());

    if !cell_ids.is_empty() {
        // Notify frontend to clear outputs before queue state updates arrive
        let _ = app.emit("cells:outputs_cleared", &cell_ids);

        queue_tx
            .send(QueueCommand::EnqueueAll {
                cell_ids: cell_ids.clone(),
            })
            .await
            .map_err(|e| e.to_string())?;
    }

    Ok(cell_ids)
}

/// Restart the kernel and run all code cells.
/// Backend-coordinated: interrupt → clear queue → shutdown → clear outputs → queue all.
/// Returns the list of cell IDs that were queued.
#[tauri::command]
async fn restart_and_run_all(
    app: tauri::AppHandle,
    notebook_state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
    queue_tx: tauri::State<'_, mpsc::Sender<QueueCommand>>,
) -> Result<Vec<String>, String> {
    info!("restart_and_run_all: starting");

    // 1. Interrupt current execution and clear the queue
    queue_tx
        .send(QueueCommand::InterruptAndClear)
        .await
        .map_err(|e| e.to_string())?;

    // 2. Shutdown the kernel
    {
        let mut kernel = kernel_state.lock().await;
        kernel.shutdown().await.map_err(|e| e.to_string())?;
    }

    // 3. Emit lifecycle event so frontend knows kernel is stopped
    let event = KernelLifecycleEvent {
        state: "not_started".to_string(),
        runtime: String::new(),
        env_source: None,
        error_message: None,
    };
    let _ = app.emit("kernel:lifecycle", &event);

    // 4. Get code cell IDs and clear their outputs
    let cell_ids = {
        let mut nb = notebook_state.lock().map_err(|e| e.to_string())?;
        let ids = nb.get_code_cell_ids();
        for id in &ids {
            nb.clear_cell_outputs(id);
        }
        ids
    };
    info!("restart_and_run_all: queuing {} cells", cell_ids.len());

    // 5. Notify frontend to clear outputs before queue state updates arrive
    if !cell_ids.is_empty() {
        let _ = app.emit("cells:outputs_cleared", &cell_ids);
    }

    // 6. Queue all code cells — the queue processor will retry until kernel is ready
    if !cell_ids.is_empty() {
        queue_tx
            .send(QueueCommand::EnqueueAll {
                cell_ids: cell_ids.clone(),
            })
            .await
            .map_err(|e| e.to_string())?;
    }

    Ok(cell_ids)
}

#[tauri::command]
async fn start_kernel(
    kernelspec_name: String,
    app: tauri::AppHandle,
    notebook_state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
) -> Result<(), String> {
    let notebook_path = {
        let state = notebook_state.lock().map_err(|e| e.to_string())?;
        state.path.clone()
    };
    let mut kernel = kernel_state.lock().await;
    kernel
        .start(app, &kernelspec_name, notebook_path.as_deref())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn interrupt_kernel(
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
) -> Result<(), String> {
    let kernel = kernel_state.lock().await;
    kernel.interrupt().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn shutdown_kernel(
    app: tauri::AppHandle,
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
) -> Result<(), String> {
    let mut kernel = kernel_state.lock().await;
    kernel.shutdown().await.map_err(|e| e.to_string())?;

    let event = KernelLifecycleEvent {
        state: "not started".to_string(),
        runtime: String::new(),
        env_source: None,
        error_message: None,
    };
    let _ = app.emit("kernel:lifecycle", &event);

    Ok(())
}

#[tauri::command]
async fn send_shell_message(
    message: serde_json::Value,
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
) -> Result<(), String> {
    let mut kernel = kernel_state.lock().await;
    kernel
        .send_shell_message(message)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_preferred_kernelspec(
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<Option<String>, String> {
    let state = state.lock().map_err(|e| e.to_string())?;
    Ok(state
        .notebook
        .metadata
        .kernelspec
        .as_ref()
        .map(|k| k.name.clone()))
}

#[tauri::command]
async fn complete(
    code: String,
    cursor_pos: usize,
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
) -> Result<CompletionResult, String> {
    let mut kernel = kernel_state.lock().await;
    kernel
        .complete(&code, cursor_pos)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_history(
    pattern: Option<String>,
    n: Option<i32>,
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
) -> Result<HistoryResult, String> {
    let mut kernel = kernel_state.lock().await;
    kernel
        .history(pattern.as_deref(), n.unwrap_or(100))
        .await
        .map_err(|e| e.to_string())
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
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<Option<NotebookDependenciesJson>, String> {
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
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<(), String> {
    let mut state = state.lock().map_err(|e| e.to_string())?;

    let uv_value = serde_json::json!({
        "dependencies": dependencies,
        "requires-python": requires_python,
    });
    state
        .notebook
        .metadata
        .additional
        .insert("uv".to_string(), uv_value);
    state.dirty = true;

    Ok(())
}

/// Add a single dependency to the notebook.
#[tauri::command]
async fn add_dependency(
    package: String,
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<(), String> {
    let mut state = state.lock().map_err(|e| e.to_string())?;

    // Get existing deps or create new
    let mut deps = uv_env::extract_dependencies(&state.notebook.metadata)
        .map(|d| d.dependencies)
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

        let requires_python =
            uv_env::extract_dependencies(&state.notebook.metadata).and_then(|d| d.requires_python);

        let uv_value = serde_json::json!({
            "dependencies": deps,
            "requires-python": requires_python,
        });
        state
            .notebook
            .metadata
            .additional
            .insert("uv".to_string(), uv_value);
        state.dirty = true;
    }

    Ok(())
}

/// Remove a dependency from the notebook.
#[tauri::command]
async fn remove_dependency(
    package: String,
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<(), String> {
    let mut state = state.lock().map_err(|e| e.to_string())?;

    let existing = uv_env::extract_dependencies(&state.notebook.metadata);
    if let Some(existing) = existing {
        // Remove by package name (ignoring version specifiers)
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

        let uv_value = serde_json::json!({
            "dependencies": deps,
            "requires-python": existing.requires_python,
        });
        state
            .notebook
            .metadata
            .additional
            .insert("uv".to_string(), uv_value);
        state.dirty = true;
    }

    Ok(())
}

/// Remove an entire dependency metadata section ("uv" or "conda") from the notebook.
///
/// Used when a notebook has both uv and conda inline dependencies and the user
/// chooses which one to keep — the other section is removed entirely.
#[tauri::command]
async fn clear_dependency_section(
    section: String,
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<(), String> {
    if section != "uv" && section != "conda" {
        return Err(format!(
            "Invalid section: {}. Must be 'uv' or 'conda'.",
            section
        ));
    }

    let mut state = state.lock().map_err(|e| e.to_string())?;
    if state
        .notebook
        .metadata
        .additional
        .remove(&section)
        .is_some()
    {
        state.dirty = true;
    }

    Ok(())
}

/// Start kernel with uv-managed environment.
#[tauri::command]
async fn start_kernel_with_uv(
    app: tauri::AppHandle,
    notebook_state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
) -> Result<(), String> {
    let (deps, env_id, notebook_path) = {
        let state = notebook_state.lock().map_err(|e| e.to_string())?;
        (
            uv_env::extract_dependencies(&state.notebook.metadata),
            uv_env::extract_env_id(&state.notebook.metadata),
            state.path.clone(),
        )
    };

    let deps = deps.ok_or_else(|| "No dependencies in notebook metadata".to_string())?;

    info!(
        "Starting uv-managed kernel with {} dependencies",
        deps.dependencies.len()
    );

    let mut kernel = kernel_state.lock().await;
    kernel
        .start_with_uv(app, &deps, env_id.as_deref(), notebook_path.as_deref())
        .await
        .map_err(|e| e.to_string())
}

/// Check if a kernel is currently running.
#[tauri::command]
async fn is_kernel_running(
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
) -> Result<bool, String> {
    let kernel = kernel_state.lock().await;
    Ok(kernel.is_running())
}

/// Get the current kernel lifecycle state for frontend status display.
/// Returns "launching" if auto-launch is in progress, "running" if kernel is running,
/// or "not_started" otherwise.
#[tauri::command]
async fn get_kernel_lifecycle(
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
    auto_launch_in_progress: tauri::State<'_, Arc<AtomicBool>>,
) -> Result<String, String> {
    // Check if auto-launch is in progress first
    if auto_launch_in_progress.load(Ordering::SeqCst) {
        return Ok("launching".to_string());
    }
    // Then check if kernel is running
    let kernel = kernel_state.lock().await;
    if kernel.is_running() {
        Ok("running".to_string())
    } else {
        Ok("not_started".to_string())
    }
}

/// Check if the running kernel has a uv-managed environment.
#[tauri::command]
async fn kernel_has_uv_env(
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
) -> Result<bool, String> {
    let kernel = kernel_state.lock().await;
    Ok(kernel.has_uv_environment())
}

/// Get the sync state between declared dependencies and the running kernel's environment.
#[tauri::command]
async fn get_env_sync_state(
    notebook_state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
) -> Result<EnvSyncState, String> {
    let declared_deps = {
        let state = notebook_state.lock().map_err(|e| e.to_string())?;
        uv_env::extract_dependencies(&state.notebook.metadata)
            .map(|d| d.dependencies)
            .unwrap_or_default()
    };

    let kernel = kernel_state.lock().await;

    if !kernel.is_running() {
        return Ok(EnvSyncState::NotRunning);
    }

    if !kernel.has_uv_environment() {
        return Ok(EnvSyncState::NotUvManaged);
    }

    let synced_deps = kernel.synced_dependencies().cloned().unwrap_or_default();

    // Compare as sets
    let declared_set: HashSet<_> = declared_deps.iter().collect();
    let synced_set: HashSet<_> = synced_deps.iter().collect();

    let added: Vec<_> = declared_set
        .difference(&synced_set)
        .map(|s| (*s).clone())
        .collect();
    let removed: Vec<_> = synced_set
        .difference(&declared_set)
        .map(|s| (*s).clone())
        .collect();

    if added.is_empty() && removed.is_empty() {
        Ok(EnvSyncState::Synced)
    } else {
        Ok(EnvSyncState::Dirty { added, removed })
    }
}

/// Sync dependencies to the running kernel's uv environment.
///
/// Installs any new/changed dependencies into the existing venv.
/// Returns true if sync was performed, false if no uv environment exists.
#[tauri::command]
async fn sync_kernel_dependencies(
    notebook_state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
) -> Result<bool, String> {
    let deps = {
        let state = notebook_state.lock().map_err(|e| e.to_string())?;
        uv_env::extract_dependencies(&state.notebook.metadata)
    };

    let Some(deps) = deps else {
        return Ok(false);
    };

    let mut kernel = kernel_state.lock().await;
    let Some(env) = kernel.uv_environment() else {
        return Ok(false);
    };

    info!(
        "Syncing {} dependencies to kernel environment",
        deps.dependencies.len()
    );

    uv_env::sync_dependencies(env, &deps.dependencies)
        .await
        .map_err(|e| e.to_string())?;

    // Update tracked synced dependencies after successful sync
    kernel.set_synced_dependencies(deps.dependencies.clone());

    Ok(true)
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
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<Option<CondaDependenciesJson>, String> {
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
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<(), String> {
    let mut state = state.lock().map_err(|e| e.to_string())?;

    let conda_value = serde_json::json!({
        "dependencies": dependencies,
        "channels": channels,
        "python": python,
    });
    state
        .notebook
        .metadata
        .additional
        .insert("conda".to_string(), conda_value);
    state.dirty = true;

    Ok(())
}

/// Add a single conda dependency to the notebook.
#[tauri::command]
async fn add_conda_dependency(
    package: String,
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<(), String> {
    let mut state = state.lock().map_err(|e| e.to_string())?;

    // Get existing deps or create new
    let existing = conda_env::extract_dependencies(&state.notebook.metadata);
    let mut deps = existing
        .as_ref()
        .map(|d| d.dependencies.clone())
        .unwrap_or_default();
    let channels = existing
        .as_ref()
        .map(|d| d.channels.clone())
        .unwrap_or_default();
    let python = existing.as_ref().and_then(|d| d.python.clone());

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

        let conda_value = serde_json::json!({
            "dependencies": deps,
            "channels": channels,
            "python": python,
        });
        state
            .notebook
            .metadata
            .additional
            .insert("conda".to_string(), conda_value);
        state.dirty = true;
    }

    Ok(())
}

/// Remove a conda dependency from the notebook.
#[tauri::command]
async fn remove_conda_dependency(
    package: String,
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<(), String> {
    let mut state = state.lock().map_err(|e| e.to_string())?;

    let existing = conda_env::extract_dependencies(&state.notebook.metadata);
    if let Some(existing) = existing {
        // Remove by package name (ignoring version specifiers)
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

        let conda_value = serde_json::json!({
            "dependencies": deps,
            "channels": existing.channels,
            "python": existing.python,
        });
        state
            .notebook
            .metadata
            .additional
            .insert("conda".to_string(), conda_value);
        state.dirty = true;
    }

    Ok(())
}

/// Start kernel with conda-managed environment.
#[tauri::command]
async fn start_kernel_with_conda(
    app: tauri::AppHandle,
    notebook_state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
) -> Result<(), String> {
    let (deps, notebook_path) = {
        let mut state = notebook_state.lock().map_err(|e| e.to_string())?;
        let mut deps = conda_env::extract_dependencies(&state.notebook.metadata)
            .ok_or_else(|| "No conda dependencies in notebook metadata".to_string())?;

        // Get or create env_id for isolation
        let env_id = state
            .notebook
            .metadata
            .additional
            .get("runt")
            .and_then(|v| v.get("env_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        if let Some(id) = env_id {
            deps.env_id = Some(id);
        } else {
            // Generate and store a new env_id
            let new_id = uuid::Uuid::new_v4().to_string();
            let runt_value = serde_json::json!({
                "env_id": new_id,
            });
            state
                .notebook
                .metadata
                .additional
                .insert("runt".to_string(), runt_value);
            state.dirty = true;
            deps.env_id = Some(new_id);
        }

        (deps, state.path.clone())
    };

    info!(
        "Starting conda-managed kernel with {} dependencies (env_id: {:?})",
        deps.dependencies.len(),
        deps.env_id
    );

    let mut kernel = kernel_state.lock().await;
    kernel
        .start_with_conda(app, &deps, notebook_path.as_deref())
        .await
        .map_err(|e| e.to_string())
}

/// Start a default uv kernel with just Python (no extra deps).
/// Used as the default when no environment is configured.
/// Uses prewarmed environments from the pool when available for faster startup.
#[tauri::command]
async fn start_default_uv_kernel(
    app: tauri::AppHandle,
    notebook_state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
    pool: tauri::State<'_, env_pool::SharedEnvPool>,
) -> Result<(), String> {
    // Ensure uv metadata exists in the notebook (for legacy notebooks)
    // Also extract env_id for per-notebook isolation
    let (env_id, notebook_path) = {
        let mut state = notebook_state.lock().map_err(|e| e.to_string())?;

        if !state.notebook.metadata.additional.contains_key("uv") {
            state.notebook.metadata.additional.insert(
                "uv".to_string(),
                serde_json::json!({
                    "dependencies": Vec::<String>::new(),
                }),
            );
            state.dirty = true;
        }

        (
            uv_env::extract_env_id(&state.notebook.metadata),
            state.path.clone(),
        )
    };

    // Try to use a prewarmed environment (daemon first, then in-process pool)
    if let Some(env_id) = &env_id {
        let prewarmed = {
            #[allow(clippy::needless_borrow)]
            env_pool::take_uv_env(&pool)
        }
        .await;
        if let Some(prewarmed_env) = prewarmed {
            info!("[prewarm] Using prewarmed environment for default uv kernel");

            // Try to claim and use the prewarmed env, but fall back gracefully on error
            match uv_env::claim_prewarmed_environment(prewarmed_env.into_uv_environment(), env_id)
                .await
            {
                Ok(env) => {
                    // Validate the python path exists before trying to use it
                    if env.python_path.exists() {
                        // Immediately spawn replenishment
                        env_pool::spawn_replenishment(pool.inner().clone());

                        let mut kernel = kernel_state.lock().await;
                        match kernel
                            .start_with_prewarmed_uv(app.clone(), env, notebook_path.as_deref())
                            .await
                        {
                            Ok(()) => return Ok(()),
                            Err(e) => {
                                log::warn!(
                                    "[prewarm] Failed to start kernel with prewarmed env, falling back: {}",
                                    e
                                );
                                // Fall through to create fresh environment
                            }
                        }
                    } else {
                        log::warn!(
                            "[prewarm] Claimed env has invalid python path: {:?}, falling back",
                            env.python_path
                        );
                    }
                }
                Err(e) => {
                    log::warn!(
                        "[prewarm] Failed to claim prewarmed env, falling back: {}",
                        e
                    );
                    // Fall through to create fresh environment
                }
            }
        }
    }

    // No prewarmed env available (or prewarmed failed), create one normally
    info!("Starting default uv kernel with ipykernel (creating fresh env)");
    let deps = uv_env::NotebookDependencies {
        dependencies: vec![],
        requires_python: None,
    };

    let mut kernel = kernel_state.lock().await;
    kernel
        .start_with_uv(app, &deps, env_id.as_deref(), notebook_path.as_deref())
        .await
        .map_err(|e| e.to_string())
}

/// Start a default conda kernel with just Python (no extra deps).
/// Used as fallback when no environment is configured.
/// Each notebook gets its own isolated environment via a unique env_id.
#[tauri::command]
async fn start_default_conda_kernel(
    app: tauri::AppHandle,
    notebook_state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
) -> Result<(), String> {
    // Get the env_id for this notebook (should be set at notebook creation)
    // Fall back to creating one for legacy notebooks
    let (env_id, notebook_path) = {
        let mut state = notebook_state.lock().map_err(|e| e.to_string())?;

        // Check if there's already an env_id in the runt metadata
        let existing_id = state
            .notebook
            .metadata
            .additional
            .get("runt")
            .and_then(|v| v.get("env_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let env_id = match existing_id {
            Some(id) => id,
            None => {
                // Legacy notebook without env_id - generate one and set conda metadata
                let new_id = uuid::Uuid::new_v4().to_string();

                state
                    .notebook
                    .metadata
                    .additional
                    .insert("runt".to_string(), serde_json::json!({ "env_id": new_id }));

                if !state.notebook.metadata.additional.contains_key("conda") {
                    state.notebook.metadata.additional.insert(
                        "conda".to_string(),
                        serde_json::json!({
                            "dependencies": Vec::<String>::new(),
                            "channels": ["conda-forge"],
                        }),
                    );
                }

                state.dirty = true;
                new_id
            }
        };
        (env_id, state.path.clone())
    };

    // Create minimal deps with just ipykernel and the unique env_id
    let deps = conda_env::CondaDependencies {
        dependencies: vec!["ipykernel".to_string()],
        channels: vec!["conda-forge".to_string()],
        python: None,
        env_id: Some(env_id.clone()),
    };

    info!(
        "Starting default conda kernel with ipykernel (env_id: {})",
        env_id
    );

    let mut kernel = kernel_state.lock().await;
    kernel
        .start_with_conda(app, &deps, notebook_path.as_deref())
        .await
        .map_err(|e| e.to_string())
}

/// Core implementation for starting a default Python kernel.
/// Extracted to allow calling from both Tauri commands and the setup hook.
async fn start_default_python_kernel_impl(
    app: tauri::AppHandle,
    notebook_state: &Arc<Mutex<NotebookState>>,
    kernel_state: &Arc<tokio::sync::Mutex<NotebookKernel>>,
    pool: &env_pool::SharedEnvPool,
    conda_pool: &env_pool::SharedCondaEnvPool,
) -> Result<String, String> {
    let kernel_start = std::time::Instant::now();

    // Load user's preferred Python environment type from settings
    let app_settings = settings::load_settings();
    let preferred_env = app_settings.default_python_env;
    let uv_available = uv_env::check_uv_available().await;

    // Check which env type actually has dependencies in the notebook metadata
    // This overrides user preference when deps exist in only one type
    let (has_uv_deps, has_conda_deps) = {
        let state = notebook_state.lock().map_err(|e| e.to_string())?;
        let uv_deps = uv_env::extract_dependencies(&state.notebook.metadata);
        let conda_deps = conda_env::extract_dependencies(&state.notebook.metadata);
        let has_uv = uv_deps.map(|d| !d.dependencies.is_empty()).unwrap_or(false);
        let has_conda = conda_deps
            .map(|d| !d.dependencies.is_empty())
            .unwrap_or(false);
        (has_uv, has_conda)
    };

    // Determine which env type to actually use
    // Priority: 1) Use whichever has deps, 2) Fall back to user preference
    let use_uv = if has_uv_deps && !has_conda_deps {
        // UV has deps, conda doesn't - use UV regardless of preference
        if uv_available {
            info!("Using uv (has dependencies, conda is empty)");
            true
        } else {
            log::warn!("Notebook has uv dependencies but uv not available, falling back to conda");
            false
        }
    } else if has_conda_deps && !has_uv_deps {
        // Conda has deps, uv doesn't - use conda regardless of preference
        info!("Using conda (has dependencies, uv is empty)");
        false
    } else if has_uv_deps && has_conda_deps {
        // Both have deps - use user preference but warn
        log::warn!(
            "Notebook has both uv and conda dependencies, using preference: {:?}",
            preferred_env
        );
        match preferred_env {
            settings::PythonEnvType::Uv | settings::PythonEnvType::Other(_) => uv_available,
            settings::PythonEnvType::Conda => false,
        }
    } else {
        // Neither has inline deps - check for project files before falling back to prewarmed.
        // Uses "closest wins" detection: single walk-up from notebook, first match wins.
        // Tiebreaker when multiple files at same level: pyproject > pixi > environment.yml.
        let notebook_path_for_detection = {
            let state = notebook_state.lock().map_err(|e| e.to_string())?;
            state.path.clone()
        };

        // Build the set of project file kinds to search for
        let mut search_kinds = vec![
            project_file::ProjectFileKind::PixiToml,
            project_file::ProjectFileKind::EnvironmentYml,
        ];
        if uv_available {
            // Only search for pyproject.toml when uv is available to handle it
            search_kinds.insert(0, project_file::ProjectFileKind::PyprojectToml);
        }

        if let Some(ref nb_path) = notebook_path_for_detection {
            if let Some(detected) = project_file::find_nearest_project_file(nb_path, &search_kinds)
            {
                match detected.kind {
                    project_file::ProjectFileKind::PyprojectToml => {
                        if let Ok(config) = pyproject::parse_pyproject(&detected.path) {
                            let info = pyproject::create_pyproject_info(&config, nb_path);
                            if info.has_dependencies || info.has_venv {
                                let project_dir = detected
                                    .path
                                    .parent()
                                    .ok_or_else(|| "Invalid pyproject.toml path".to_string())?;

                                info!(
                                    "Auto-detected pyproject.toml at {} (closest project file), starting with uv run",
                                    info.relative_path
                                );

                                let mut kernel = kernel_state.lock().await;
                                kernel
                                    .start_with_uv_run(app, project_dir)
                                    .await
                                    .map_err(|e| e.to_string())?;

                                info!(
                                    "[kernel-ready] Started UV kernel in {}ms | Source: pyproject.toml (auto-detected)",
                                    kernel_start.elapsed().as_millis()
                                );
                                return Ok("uv:pyproject".to_string());
                            }
                        }
                        // Closest project file has no usable deps — fall through to prewarmed
                    }
                    project_file::ProjectFileKind::PixiToml => {
                        if let Ok(config) = pixi::parse_pixi_toml(&detected.path) {
                            if !config.dependencies.is_empty() {
                                let pixi_info = pixi::create_pixi_info(&config, nb_path);
                                info!(
                                    "Auto-detected pixi.toml at {} with {} deps (closest project file), using conda/rattler",
                                    pixi_info.relative_path,
                                    pixi_info.dependency_count
                                );

                                let mut deps = pixi::convert_to_conda_dependencies(&config);

                                // Get or create env_id for this notebook
                                let env_id = {
                                    let mut state =
                                        notebook_state.lock().map_err(|e| e.to_string())?;
                                    let existing_id = state
                                        .notebook
                                        .metadata
                                        .additional
                                        .get("runt")
                                        .and_then(|v| v.get("env_id"))
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string());
                                    match existing_id {
                                        Some(id) => id,
                                        None => {
                                            let new_id = uuid::Uuid::new_v4().to_string();
                                            state.notebook.metadata.additional.insert(
                                                "runt".to_string(),
                                                serde_json::json!({ "env_id": new_id }),
                                            );
                                            state.dirty = true;
                                            new_id
                                        }
                                    }
                                };
                                deps.env_id = Some(env_id);

                                let mut kernel = kernel_state.lock().await;
                                kernel
                                    .start_with_conda(
                                        app,
                                        &deps,
                                        notebook_path_for_detection.as_deref(),
                                    )
                                    .await
                                    .map_err(|e| e.to_string())?;

                                info!(
                                    "[kernel-ready] Started Conda kernel in {}ms | Source: pixi.toml (auto-detected)",
                                    kernel_start.elapsed().as_millis()
                                );
                                return Ok("conda:pixi".to_string());
                            }
                        }
                        // Closest project file has no usable deps — fall through to prewarmed
                    }
                    project_file::ProjectFileKind::EnvironmentYml => {
                        if let Ok(config) = environment_yml::parse_environment_yml(&detected.path) {
                            if !config.dependencies.is_empty() {
                                let deps = environment_yml::convert_to_conda_dependencies(&config);
                                info!(
                                    "Auto-detected environment.yml at {} with {} deps (closest project file)",
                                    detected.path.display(),
                                    deps.dependencies.len()
                                );
                                let mut kernel = kernel_state.lock().await;
                                kernel
                                    .start_with_conda(app, &deps, Some(nb_path))
                                    .await
                                    .map_err(|e| e.to_string())?;

                                info!(
                                    "[kernel-ready] Started conda kernel via environment.yml in {}ms",
                                    kernel_start.elapsed().as_millis()
                                );
                                return Ok("conda:env_yml".to_string());
                            }
                        }
                        // Closest project file has no usable deps — fall through to prewarmed
                    }
                }
            }
        }

        // No project file found (or closest had no usable deps) — fall back to user preference
        match preferred_env {
            settings::PythonEnvType::Uv | settings::PythonEnvType::Other(_) => {
                if uv_available {
                    true
                } else {
                    info!("uv preferred but not available, falling back to conda");
                    false
                }
            }
            settings::PythonEnvType::Conda => false,
        }
    };

    if use_uv {
        info!(
            "Using uv for default kernel (preferred: {:?})",
            preferred_env
        );

        // Ensure uv metadata exists in the notebook (for legacy notebooks)
        // Also extract env_id for per-notebook isolation and notebook dependencies
        let (env_id, notebook_path, notebook_deps) = {
            let mut state = notebook_state.lock().map_err(|e| e.to_string())?;

            // If notebook has empty conda deps and we're using UV, migrate to UV
            let should_setup_uv =
                if let Some(conda_val) = state.notebook.metadata.additional.get("conda") {
                    // Only migrate if conda deps are empty
                    let conda_deps = conda_val
                        .get("dependencies")
                        .and_then(|d| d.as_array())
                        .map(|a| a.is_empty())
                        .unwrap_or(true);
                    conda_deps
                } else {
                    true
                };

            if should_setup_uv && !state.notebook.metadata.additional.contains_key("uv") {
                state.notebook.metadata.additional.insert(
                    "uv".to_string(),
                    serde_json::json!({
                        "dependencies": Vec::<String>::new(),
                    }),
                );
                // Remove empty conda metadata if migrating to uv
                if let Some(conda_val) = state.notebook.metadata.additional.get("conda") {
                    let conda_deps_empty = conda_val
                        .get("dependencies")
                        .and_then(|d| d.as_array())
                        .map(|a| a.is_empty())
                        .unwrap_or(true);
                    if conda_deps_empty {
                        state.notebook.metadata.additional.remove("conda");
                    }
                }
                state.dirty = true;
            }

            // Extract notebook dependencies
            let deps = uv_env::extract_dependencies(&state.notebook.metadata);

            (
                uv_env::extract_env_id(&state.notebook.metadata),
                state.path.clone(),
                deps,
            )
        };

        // Check if notebook has dependencies - if so, use prepare_environment path
        // which properly finds existing cached environments
        let has_deps = notebook_deps
            .as_ref()
            .map(|d| !d.dependencies.is_empty())
            .unwrap_or(false);

        if has_deps {
            // Notebook has dependencies - use the normal path that finds/creates cached envs
            let deps = notebook_deps.unwrap();
            info!(
                "[env] Notebook has {} dependencies, using prepare_environment path",
                deps.dependencies.len()
            );

            let mut kernel = kernel_state.lock().await;
            kernel
                .start_with_uv(app, &deps, env_id.as_deref(), notebook_path.as_deref())
                .await
                .map_err(|e| e.to_string())?;

            info!(
                "[kernel-ready] Started UV kernel in {}ms | Source: cached/fresh env with deps",
                kernel_start.elapsed().as_millis()
            );
            return Ok("uv:inline".to_string());
        }

        // No dependencies - try to use a prewarmed environment (daemon first, then in-process pool)
        if let Some(env_id) = &env_id {
            let prewarmed = {
                #[allow(clippy::needless_borrow)]
                env_pool::take_uv_env(&pool)
            }
            .await;
            if let Some(prewarmed_env) = prewarmed {
                info!("[prewarm] Using prewarmed environment for notebook (no deps)");

                // Try to claim and use the prewarmed env, but fall back gracefully on error
                match uv_env::claim_prewarmed_environment(
                    prewarmed_env.into_uv_environment(),
                    env_id,
                )
                .await
                {
                    Ok(env) => {
                        // Validate the python path exists before trying to use it
                        if env.python_path.exists() {
                            // Immediately spawn replenishment
                            env_pool::spawn_replenishment(pool.clone());

                            let mut kernel = kernel_state.lock().await;
                            match kernel
                                .start_with_prewarmed_uv(app.clone(), env, notebook_path.as_deref())
                                .await
                            {
                                Ok(()) => {
                                    info!(
                                        "[kernel-ready] Started UV kernel in {}ms | Source: prewarmed",
                                        kernel_start.elapsed().as_millis()
                                    );
                                    return Ok("uv:prewarmed".to_string());
                                }
                                Err(e) => {
                                    log::warn!(
                                        "[prewarm] Failed to start kernel with prewarmed env, falling back: {}",
                                        e
                                    );
                                    // Fall through to create fresh environment
                                }
                            }
                        } else {
                            log::warn!(
                                "[prewarm] Claimed env has invalid python path: {:?}, falling back",
                                env.python_path
                            );
                        }
                    }
                    Err(e) => {
                        log::warn!(
                            "[prewarm] Failed to claim prewarmed env, falling back: {}",
                            e
                        );
                        // Fall through to create fresh environment
                    }
                }
            }
        }

        // No prewarmed env available, create one normally
        info!("[prewarm:uv] No prewarmed environment available, creating fresh");

        // Include default uv packages from settings
        let default_deps: Vec<String> = {
            let s = settings::load_settings();
            s.uv.default_packages
        };
        if !default_deps.is_empty() {
            info!(
                "[prewarm:uv] Including default packages: {:?}",
                default_deps
            );
        }

        let deps = uv_env::NotebookDependencies {
            dependencies: default_deps,
            requires_python: None,
        };

        let mut kernel = kernel_state.lock().await;
        kernel
            .start_with_uv(app, &deps, env_id.as_deref(), notebook_path.as_deref())
            .await
            .map_err(|e| e.to_string())?;

        info!(
            "[kernel-ready] Started UV kernel in {}ms | Source: fresh",
            kernel_start.elapsed().as_millis()
        );
        Ok("uv:fresh".to_string())
    } else {
        info!(
            "Using conda/rattler for default kernel (preferred: {:?})",
            preferred_env
        );

        // Get the env_id for this notebook (should be set at notebook creation)
        // Fall back to creating one for legacy notebooks
        // Also extract conda dependencies
        let (env_id, notebook_path, conda_deps) = {
            let mut state = notebook_state.lock().map_err(|e| e.to_string())?;

            // Check if notebook has empty uv deps - if so, migrate to conda
            let uv_deps_empty = state
                .notebook
                .metadata
                .additional
                .get("uv")
                .and_then(|v| v.get("dependencies"))
                .and_then(|d| d.as_array())
                .map(|a| a.is_empty())
                .unwrap_or(true);

            // Remove empty uv metadata when migrating to conda
            if uv_deps_empty && state.notebook.metadata.additional.contains_key("uv") {
                state.notebook.metadata.additional.remove("uv");
                state.dirty = true;
            }

            // Check if there's already an env_id in the runt metadata
            let existing_id = state
                .notebook
                .metadata
                .additional
                .get("runt")
                .and_then(|v| v.get("env_id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let env_id = match existing_id {
                Some(id) => {
                    // Ensure conda metadata exists even for existing notebooks
                    if !state.notebook.metadata.additional.contains_key("conda") {
                        state.notebook.metadata.additional.insert(
                            "conda".to_string(),
                            serde_json::json!({
                                "dependencies": Vec::<String>::new(),
                                "channels": ["conda-forge"],
                            }),
                        );
                        state.dirty = true;
                    }
                    id
                }
                None => {
                    // Legacy notebook without env_id - generate one and set conda metadata
                    let new_id = uuid::Uuid::new_v4().to_string();

                    state
                        .notebook
                        .metadata
                        .additional
                        .insert("runt".to_string(), serde_json::json!({ "env_id": new_id }));

                    if !state.notebook.metadata.additional.contains_key("conda") {
                        state.notebook.metadata.additional.insert(
                            "conda".to_string(),
                            serde_json::json!({
                                "dependencies": Vec::<String>::new(),
                                "channels": ["conda-forge"],
                            }),
                        );
                    }

                    state.dirty = true;
                    new_id
                }
            };

            // Extract conda dependencies
            let deps = conda_env::extract_dependencies(&state.notebook.metadata);

            (env_id, state.path.clone(), deps)
        };

        // Check if notebook has dependencies - if so, use prepare_environment path
        // which properly finds existing cached environments
        let has_deps = conda_deps
            .as_ref()
            .map(|d| !d.dependencies.is_empty())
            .unwrap_or(false);

        if has_deps {
            // Notebook has dependencies - use the normal path that finds/creates cached envs
            let mut deps = conda_deps.unwrap();
            deps.env_id = Some(env_id.clone());
            info!(
                "[env] Notebook has {} conda dependencies, using prepare_environment path",
                deps.dependencies.len()
            );

            let mut kernel = kernel_state.lock().await;
            kernel
                .start_with_conda(app, &deps, notebook_path.as_deref())
                .await
                .map_err(|e| e.to_string())?;

            info!(
                "[kernel-ready] Started Conda kernel in {}ms | Source: cached/fresh env with deps",
                kernel_start.elapsed().as_millis()
            );
            return Ok("conda:inline".to_string());
        }

        // No dependencies - try to use a prewarmed conda environment (daemon first, then in-process pool)
        let prewarmed = {
            #[allow(clippy::needless_borrow)]
            env_pool::take_conda_env(&conda_pool)
        }
        .await;
        if let Some(prewarmed_env) = prewarmed {
            info!("[prewarm] Using prewarmed conda environment for notebook (no deps)");

            // Try to claim and use the prewarmed env, but fall back gracefully on error
            match conda_env::claim_prewarmed_conda_environment(
                prewarmed_env.into_conda_environment(),
                &env_id,
            )
            .await
            {
                Ok(env) => {
                    if env.python_path.exists() {
                        let mut kernel = kernel_state.lock().await;
                        match kernel
                            .start_with_prewarmed_conda(app.clone(), env, notebook_path.as_deref())
                            .await
                        {
                            Ok(()) => {
                                // Trigger replenishment of the pool
                                env_pool::spawn_conda_replenishment(conda_pool.clone());
                                info!(
                                    "[kernel-ready] Started Conda kernel in {}ms | Source: prewarmed",
                                    kernel_start.elapsed().as_millis()
                                );
                                return Ok("conda:prewarmed".to_string());
                            }
                            Err(e) => {
                                error!(
                                    "[prewarm] Failed to start kernel with prewarmed conda env, falling back: {}",
                                    e
                                );
                            }
                        }
                    } else {
                        info!(
                            "[prewarm] Claimed conda env has invalid python path: {:?}, falling back",
                            env.python_path
                        );
                    }
                }
                Err(e) => {
                    error!(
                        "[prewarm] Failed to claim prewarmed conda env, falling back: {}",
                        e
                    );
                }
            }
        }

        // No prewarmed env available (or prewarmed failed), create one normally
        info!("[prewarm:conda] No prewarmed conda environment available, creating fresh");

        // Include default conda packages from settings
        let mut conda_deps_list = vec!["ipykernel".to_string()];
        {
            let s = settings::load_settings();
            let extra = s.conda.default_packages;
            if !extra.is_empty() {
                info!("[prewarm:conda] Including default packages: {:?}", extra);
                conda_deps_list.extend(extra);
            }
        }

        let deps = conda_env::CondaDependencies {
            dependencies: conda_deps_list,
            channels: vec!["conda-forge".to_string()],
            python: None,
            env_id: Some(env_id.clone()),
        };

        let mut kernel = kernel_state.lock().await;
        kernel
            .start_with_conda(app, &deps, notebook_path.as_deref())
            .await
            .map_err(|e| e.to_string())?;

        info!(
            "[kernel-ready] Started Conda kernel in {}ms | Source: fresh",
            kernel_start.elapsed().as_millis()
        );
        Ok("conda:fresh".to_string())
    }
}

/// Start a default kernel, automatically choosing uv or conda based on availability.
/// Prefers uv when available, falls back to conda/rattler otherwise.
/// Uses prewarmed environments from the pool when available for faster startup.
#[tauri::command]
async fn start_default_kernel(
    app: tauri::AppHandle,
    notebook_state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
    pool: tauri::State<'_, env_pool::SharedEnvPool>,
    conda_pool: tauri::State<'_, env_pool::SharedCondaEnvPool>,
) -> Result<String, String> {
    start_default_python_kernel_impl(app, &notebook_state, &kernel_state, &pool, &conda_pool).await
}

/// Check if the running kernel has a conda-managed environment.
#[tauri::command]
async fn kernel_has_conda_env(
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
) -> Result<bool, String> {
    let kernel = kernel_state.lock().await;
    Ok(kernel.has_conda_environment())
}

/// Sync dependencies to the running kernel's conda environment.
///
/// Note: For conda environments, this currently requires a kernel restart.
/// Returns true if sync was performed, false if no conda environment exists.
#[tauri::command]
async fn sync_conda_dependencies(
    notebook_state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
) -> Result<bool, String> {
    let deps = {
        let state = notebook_state.lock().map_err(|e| e.to_string())?;
        conda_env::extract_dependencies(&state.notebook.metadata)
    };

    let Some(deps) = deps else {
        return Ok(false);
    };

    let kernel = kernel_state.lock().await;
    let Some(env) = kernel.conda_environment() else {
        return Ok(false);
    };

    info!(
        "Syncing {} conda dependencies to kernel environment",
        deps.dependencies.len()
    );

    // Note: This will return an error for now since conda sync requires restart
    conda_env::sync_dependencies(env, &deps)
        .await
        .map_err(|e| e.to_string())?;

    Ok(true)
}

// ============================================================================
// pyproject.toml Discovery and Environment Commands
// ============================================================================

/// Detect pyproject.toml near the notebook and return info about it.
#[tauri::command]
async fn detect_pyproject(
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<Option<pyproject::PyProjectInfo>, String> {
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
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<Option<PyProjectDepsJson>, String> {
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
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<(), String> {
    let notebook_path = {
        let state = state.lock().map_err(|e| e.to_string())?;
        state.path.clone()
    };

    let Some(notebook_path) = notebook_path else {
        return Err("No notebook path set".to_string());
    };

    let Some(pyproject_path) = pyproject::find_pyproject(&notebook_path) else {
        return Err("No pyproject.toml found".to_string());
    };

    let config = pyproject::parse_pyproject(&pyproject_path).map_err(|e| e.to_string())?;

    // Merge pyproject deps into notebook metadata
    let mut state = state.lock().map_err(|e| e.to_string())?;

    let all_deps = pyproject::get_all_dependencies(&config);

    let uv_value = serde_json::json!({
        "dependencies": all_deps,
        "requires-python": config.requires_python,
    });

    state
        .notebook
        .metadata
        .additional
        .insert("uv".to_string(), uv_value);
    state.dirty = true;

    info!(
        "Imported {} dependencies from pyproject.toml into notebook",
        all_deps.len()
    );

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
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<trust::TrustInfo, String> {
    let state = state.lock().map_err(|e| e.to_string())?;
    trust::verify_notebook_trust(&state.notebook.metadata.additional)
}

/// Approve the notebook's dependencies and sign them with the local trust key.
///
/// After calling this, the notebook will be trusted on subsequent opens (until
/// the dependency metadata is modified externally).
#[tauri::command]
async fn approve_notebook_trust(
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<(), String> {
    let mut state = state.lock().map_err(|e| e.to_string())?;

    // Compute signature over current dependencies
    let signature = trust::sign_notebook_dependencies(&state.notebook.metadata.additional)?;

    // Get or create the runt metadata section
    let runt_value = state
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

    state.dirty = true;
    Ok(())
}

/// Check packages for typosquatting (similar names to popular packages).
///
/// Returns warnings for any packages that look like potential typosquats.
#[tauri::command]
async fn check_typosquats(packages: Vec<String>) -> Vec<typosquat::TyposquatWarning> {
    typosquat::check_packages(&packages)
}

/// Start kernel using `uv run` in the project directory with pyproject.toml.
///
/// This delegates environment management to uv:
/// - uv auto-detects and uses the project's pyproject.toml
/// - Creates/updates .venv in the project directory
/// - Respects uv.lock if present
/// - Adds ipykernel transiently via --with
#[tauri::command]
async fn start_kernel_with_pyproject(
    app: tauri::AppHandle,
    notebook_state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
) -> Result<(), String> {
    let notebook_path = {
        let state = notebook_state.lock().map_err(|e| e.to_string())?;
        state.path.clone()
    };

    let notebook_path = notebook_path.ok_or_else(|| "No notebook path set".to_string())?;

    let pyproject_path = pyproject::find_pyproject(&notebook_path)
        .ok_or_else(|| "No pyproject.toml found".to_string())?;

    // Get the project directory (parent of pyproject.toml)
    let project_dir = pyproject_path
        .parent()
        .ok_or_else(|| "Invalid pyproject.toml path".to_string())?;

    info!(
        "Starting kernel with uv run in project {}",
        project_dir.display()
    );

    let mut kernel = kernel_state.lock().await;
    kernel
        .start_with_uv_run(app, project_dir)
        .await
        .map_err(|e| e.to_string())
}

// ============================================================================
// pixi.toml Discovery and Environment Commands
// ============================================================================

/// Detect pixi.toml near the notebook and return info about it.
#[tauri::command]
async fn detect_pixi_toml(
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<Option<pixi::PixiInfo>, String> {
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
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<Option<PixiDepsJson>, String> {
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
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<Option<environment_yml::EnvironmentYmlInfo>, String> {
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
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<Option<EnvironmentYmlDepsJson>, String> {
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

/// Start kernel using dependencies from a detected environment.yml.
#[tauri::command]
async fn start_kernel_with_environment_yml(
    app: tauri::AppHandle,
    notebook_state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
) -> Result<(), String> {
    let notebook_path = {
        let state = notebook_state.lock().map_err(|e| e.to_string())?;
        state.path.clone()
    };

    let Some(notebook_path) = notebook_path else {
        return Err("No notebook path available".to_string());
    };

    let Some(yml_path) = environment_yml::find_environment_yml(&notebook_path) else {
        return Err("No environment.yml found".to_string());
    };

    let config = environment_yml::parse_environment_yml(&yml_path).map_err(|e| e.to_string())?;
    let deps = environment_yml::convert_to_conda_dependencies(&config);

    info!(
        "Starting kernel with environment.yml ({} deps) from {}",
        deps.dependencies.len(),
        yml_path.display()
    );

    let mut kernel = kernel_state.lock().await;
    kernel
        .start_with_conda(app, &deps, Some(&notebook_path))
        .await
        .map_err(|e| e.to_string())
}

/// Import dependencies from pixi.toml into notebook conda metadata.
/// This converts pixi deps to conda format and stores them inline in the notebook.
#[tauri::command]
async fn import_pixi_dependencies(
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<(), String> {
    let notebook_path = {
        let state = state.lock().map_err(|e| e.to_string())?;
        state.path.clone()
    };

    let Some(notebook_path) = notebook_path else {
        return Err("No notebook path set".to_string());
    };

    let Some(pixi_path) = pixi::find_pixi_toml(&notebook_path) else {
        return Err("No pixi.toml found".to_string());
    };

    let config = pixi::parse_pixi_toml(&pixi_path).map_err(|e| e.to_string())?;
    let conda_deps = pixi::convert_to_conda_dependencies(&config);

    // Merge pixi deps into notebook conda metadata
    let mut state = state.lock().map_err(|e| e.to_string())?;

    let mut conda_value = serde_json::json!({
        "dependencies": conda_deps.dependencies,
        "channels": conda_deps.channels,
    });
    if let Some(python) = &conda_deps.python {
        conda_value["python"] = serde_json::json!(python);
    }

    state
        .notebook
        .metadata
        .additional
        .insert("conda".to_string(), conda_value);
    state.dirty = true;

    info!(
        "Imported {} dependencies from pixi.toml into notebook conda metadata",
        conda_deps.dependencies.len()
    );

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
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<String, String> {
    let state = state.lock().map_err(|e| e.to_string())?;
    Ok(state.get_runtime().to_string())
}

/// Detect deno.json/deno.jsonc near the notebook and return info about it
#[tauri::command]
async fn detect_deno_config(
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<Option<deno_env::DenoConfigInfo>, String> {
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
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<Vec<String>, String> {
    let state = state.lock().map_err(|e| e.to_string())?;
    let deps = deno_env::extract_deno_metadata(&state.notebook.metadata);
    Ok(deps.map(|d| d.permissions).unwrap_or_default())
}

/// Set Deno permissions in notebook metadata
#[tauri::command]
async fn set_deno_permissions(
    permissions: Vec<String>,
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<(), String> {
    let mut state = state.lock().map_err(|e| e.to_string())?;

    // Preserve existing settings when updating permissions
    let mut deno_deps =
        deno_env::extract_deno_metadata(&state.notebook.metadata).unwrap_or_default();
    deno_deps.permissions = permissions;

    let deno_value = serde_json::to_value(&deno_deps).map_err(|e| e.to_string())?;
    state
        .notebook
        .metadata
        .additional
        .insert("deno".to_string(), deno_value);
    state.dirty = true;

    Ok(())
}

/// Get Deno flexible npm imports setting from notebook metadata
#[tauri::command]
async fn get_deno_flexible_npm_imports(
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<bool, String> {
    let state = state.lock().map_err(|e| e.to_string())?;
    let deps = deno_env::extract_deno_metadata(&state.notebook.metadata);
    Ok(deps.map(|d| d.flexible_npm_imports).unwrap_or(true))
}

/// Set Deno flexible npm imports setting in notebook metadata
#[tauri::command]
async fn set_deno_flexible_npm_imports(
    enabled: bool,
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<(), String> {
    let mut state = state.lock().map_err(|e| e.to_string())?;

    // Preserve existing settings when updating flexible_npm_imports
    let mut deno_deps =
        deno_env::extract_deno_metadata(&state.notebook.metadata).unwrap_or_default();
    deno_deps.flexible_npm_imports = enabled;

    let deno_value = serde_json::to_value(&deno_deps).map_err(|e| e.to_string())?;
    state
        .notebook
        .metadata
        .additional
        .insert("deno".to_string(), deno_value);
    state.dirty = true;

    Ok(())
}

/// Core implementation for starting a Deno kernel.
/// Extracted to allow calling from both Tauri commands and the setup hook.
async fn start_deno_kernel_impl(
    app: tauri::AppHandle,
    notebook_state: &Arc<Mutex<NotebookState>>,
    kernel_state: &Arc<tokio::sync::Mutex<NotebookKernel>>,
) -> Result<(), String> {
    // Get permissions and settings from notebook metadata
    let (permissions, workspace_dir, flexible_npm_imports, notebook_path) = {
        let state = notebook_state.lock().map_err(|e| e.to_string())?;
        let deps = deno_env::extract_deno_metadata(&state.notebook.metadata);
        let perms = deps
            .as_ref()
            .map(|d| d.permissions.clone())
            .unwrap_or_default();
        let flexible = deps.map(|d| d.flexible_npm_imports).unwrap_or(true);

        // Find workspace directory with deno.json (canonicalized so Deno gets an
        // absolute working directory even when the notebook was opened with a relative path)
        let ws_dir = state
            .path
            .as_ref()
            .and_then(|p| deno_env::find_deno_config(p))
            .and_then(|c| {
                c.parent()
                    .map(|p| p.canonicalize().unwrap_or_else(|_| p.to_path_buf()))
            });

        (perms, ws_dir, flexible, state.path.clone())
    };

    info!(
        "Starting Deno kernel with permissions: {:?}, workspace: {:?}, flexible_npm_imports: {}",
        permissions, workspace_dir, flexible_npm_imports
    );

    let mut kernel = kernel_state.lock().await;
    kernel
        .start_with_deno(
            app,
            &permissions,
            workspace_dir.as_deref(),
            flexible_npm_imports,
            notebook_path.as_deref(),
        )
        .await
        .map_err(|e| e.to_string())
}

/// Start a Deno kernel
#[tauri::command]
async fn start_kernel_with_deno(
    app: tauri::AppHandle,
    notebook_state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    kernel_state: tauri::State<'_, Arc<tokio::sync::Mutex<NotebookKernel>>>,
) -> Result<(), String> {
    start_deno_kernel_impl(app, &notebook_state, &kernel_state).await
}

/// Format a cell's source code using the appropriate formatter (ruff for Python, deno fmt for TypeScript/JavaScript).
/// Returns the formatted source and whether it changed. If formatting fails (e.g., syntax error),
/// returns the original source with an error message.
#[tauri::command]
async fn format_cell(
    cell_id: String,
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
    app: tauri::AppHandle,
) -> Result<format::FormatResult, String> {
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
        let _ = app.emit(
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
    state: tauri::State<'_, Arc<Mutex<NotebookState>>>,
) -> Result<bool, String> {
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
        "daemon_execution" => {
            let enabled = value.as_bool().ok_or("expected boolean")?;
            let mut s = settings::load_settings();
            s.daemon_execution = enabled;
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

/// Spawn a new notebook process with the specified runtime
fn spawn_new_notebook(runtime: Runtime) {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe)
            .args(["--runtime", &runtime.to_string()])
            .spawn();
    }
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

    let initial_state = match notebook_path {
        Some(ref path) if path.exists() => {
            // Existing notebook - load it (runtime comes from notebook metadata)
            let content = std::fs::read_to_string(path)?;
            let nb = nbformat::parse_notebook(&content).map_err(|e| anyhow::anyhow!("{}", e))?;
            let nb_v4 = match nb {
                nbformat::Notebook::V4(nb) => nb,
                nbformat::Notebook::Legacy(legacy) => nbformat::upgrade_legacy_notebook(legacy)?,
            };
            NotebookState::from_notebook(nb_v4, path.clone())
        }
        Some(ref path) => {
            // New notebook at specified path - detect pyproject.toml for Python
            create_new_notebook_state(path, runtime)
        }
        None => {
            // New empty notebook with requested runtime
            NotebookState::new_empty_with_runtime(runtime)
        }
    };

    let window_title = notebook_path
        .as_ref()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("Untitled.ipynb")
        .to_string();

    // Create execution queue and command sender
    let queue: SharedExecutionQueue = Arc::new(Mutex::new(ExecutionQueue::default()));
    let notebook_state = Arc::new(Mutex::new(initial_state));
    let kernel_state = Arc::new(tokio::sync::Mutex::new(NotebookKernel::default()));

    // Create the prewarming environment pools (UV and Conda)
    let env_pool: env_pool::SharedEnvPool = Arc::new(tokio::sync::Mutex::new(
        env_pool::EnvPool::new(env_pool::PoolConfig::default()),
    ));
    let conda_env_pool: env_pool::SharedCondaEnvPool = Arc::new(tokio::sync::Mutex::new(
        env_pool::CondaEnvPool::new(env_pool::PoolConfig::default()),
    ));

    // Track auto-launch state for frontend to query
    let auto_launch_in_progress = Arc::new(AtomicBool::new(false));

    // Notebook sync client for cross-window state synchronization
    let notebook_sync: SharedNotebookSync = Arc::new(tokio::sync::Mutex::new(None));

    // Recovery completion flags - set when prewarming loops finish recovery
    let uv_recovery_complete = Arc::new(AtomicBool::new(false));
    let conda_recovery_complete = Arc::new(AtomicBool::new(false));

    // Clone for setup closure
    let queue_for_processor = queue.clone();
    let notebook_for_processor = notebook_state.clone();
    let kernel_for_processor = kernel_state.clone();
    let pool_for_prewarm = env_pool.clone();
    let conda_pool_for_prewarm = conda_env_pool.clone();
    let uv_recovery_for_prewarm = uv_recovery_complete.clone();
    let conda_recovery_for_prewarm = conda_recovery_complete.clone();

    // Clone for auto-launch kernel task
    let notebook_for_autolaunch = notebook_state.clone();
    let kernel_for_autolaunch = kernel_state.clone();
    let pool_for_autolaunch = env_pool.clone();
    let conda_pool_for_autolaunch = conda_env_pool.clone();
    let auto_launch_flag = auto_launch_in_progress.clone();
    let uv_recovery_for_autolaunch = uv_recovery_complete.clone();
    let conda_recovery_for_autolaunch = conda_recovery_complete.clone();

    // Clone for lifecycle event handlers
    let kernel_for_window_event = kernel_state.clone();
    let kernel_for_exit = kernel_state.clone();
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let notebook_for_open = notebook_state.clone();

    // Clone for notebook sync initialization
    let notebook_for_sync = notebook_state.clone();
    let notebook_sync_for_init = notebook_sync.clone();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(notebook_state)
        .manage(kernel_state)
        .manage(queue)
        .manage(env_pool)
        .manage(conda_env_pool)
        .manage(auto_launch_in_progress)
        .manage(notebook_sync)
        .invoke_handler(tauri::generate_handler![
            load_notebook,
            has_notebook_path,
            get_notebook_path,
            save_notebook,
            save_notebook_as,
            clone_notebook_to_path,
            open_notebook_in_new_window,
            update_cell_source,
            add_cell,
            delete_cell,
            execute_cell,
            sync_append_output,
            sync_execution_count,
            // Daemon kernel operations (Phase 8)
            launch_kernel_via_daemon,
            queue_cell_via_daemon,
            clear_outputs_via_daemon,
            interrupt_via_daemon,
            shutdown_kernel_via_daemon,
            get_daemon_kernel_info,
            get_daemon_queue_state,
            run_all_cells_via_daemon,
            send_comm_via_daemon,
            reconnect_to_daemon,
            debug_get_automerge_state,
            debug_get_local_state,
            queue_execute_cell,
            clear_execution_queue,
            get_execution_queue_state,
            run_all_cells,
            restart_and_run_all,
            start_kernel,
            interrupt_kernel,
            shutdown_kernel,
            send_shell_message,
            complete,
            get_history,
            get_preferred_kernelspec,
            list_kernelspecs,
            // UV dependency management
            check_uv_available,
            get_notebook_dependencies,
            set_notebook_dependencies,
            add_dependency,
            remove_dependency,
            clear_dependency_section,
            start_kernel_with_uv,
            start_default_uv_kernel,
            is_kernel_running,
            get_kernel_lifecycle,
            kernel_has_uv_env,
            get_env_sync_state,
            sync_kernel_dependencies,
            // Conda dependency management
            get_conda_dependencies,
            set_conda_dependencies,
            add_conda_dependency,
            remove_conda_dependency,
            start_kernel_with_conda,
            start_default_conda_kernel,
            start_default_kernel,
            kernel_has_conda_env,
            sync_conda_dependencies,
            // pyproject.toml discovery
            detect_pyproject,
            get_pyproject_dependencies,
            import_pyproject_dependencies,
            start_kernel_with_pyproject,
            // pixi.toml support
            detect_pixi_toml,
            get_pixi_dependencies,
            import_pixi_dependencies,
            // environment.yml support
            detect_environment_yml,
            get_environment_yml_dependencies,
            start_kernel_with_environment_yml,
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
            start_kernel_with_deno,
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
        ])
        .setup(move |app| {
            let setup_start = std::time::Instant::now();
            log::info!("[startup] App setup starting");

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

            // Spawn the execution queue processor
            let app_handle = app.handle().clone();
            let queue_tx = execution_queue::spawn_queue_processor(
                app_handle,
                queue_for_processor,
                notebook_for_processor,
                kernel_for_processor.clone(),
            );

            // Store the queue sender in Tauri state for commands to use
            app.manage(queue_tx.clone());

            // Set the queue sender on the kernel so iopub can signal completion
            let kernel_for_tx = kernel_for_processor.clone();
            let tx_for_kernel = queue_tx.clone();
            tauri::async_runtime::spawn(async move {
                let mut kernel = kernel_for_tx.lock().await;
                kernel.set_queue_tx(tx_for_kernel);
            });

            // Try to ensure runtimed is running (non-blocking, optional)
            // The daemon provides centralized prewarming across all notebook windows
            let app_for_daemon = app.handle().clone();
            let app_for_sync = app.handle().clone();
            let app_for_notebook_sync = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Get path to bundled runtimed binary (for auto-installation)
                let binary_path = get_bundled_runtimed_path(&app_for_daemon);
                let daemon_available = match runtimed::client::ensure_daemon_running(binary_path).await {
                    Ok(endpoint) => {
                        log::info!("[startup] runtimed running at {}", endpoint);
                        true
                    }
                    Err(e) => {
                        // Not critical - in-process prewarming will work as fallback
                        log::info!("[startup] runtimed not available: {}. Using in-process prewarming.", e);
                        false
                    }
                };

                // Start settings sync subscription (reconnects automatically)
                // Spawn as separate task since it runs forever
                tokio::spawn(run_settings_sync(app_for_sync));

                // Initialize notebook sync if daemon is available
                if daemon_available {
                    if let Err(e) = initialize_notebook_sync(
                        app_for_notebook_sync,
                        notebook_for_sync,
                        notebook_sync_for_init,
                    )
                    .await
                    {
                        log::warn!("[startup] Notebook sync initialization failed: {}", e);
                    }
                }
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

            // Auto-launch kernel for faster startup (only if trusted)
            log::info!("[startup] Setup complete in {}ms, spawning auto-launch task", setup_start.elapsed().as_millis());
            let app_for_autolaunch = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let autolaunch_start = std::time::Instant::now();

                // Load user's preferred Python environment type
                let app_settings = settings::load_settings();
                let prefer_conda = matches!(app_settings.default_python_env, settings::PythonEnvType::Conda);

                // Wait for the PREFERRED pool recovery to complete (with timeout)
                // This ensures prewarmed environments are available before auto-launch
                let recovery_timeout = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    async {
                        if prefer_conda {
                            // Wait for conda recovery specifically
                            while !conda_recovery_for_autolaunch.load(std::sync::atomic::Ordering::SeqCst) {
                                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                            }
                        } else {
                            // Wait for UV recovery specifically
                            while !uv_recovery_for_autolaunch.load(std::sync::atomic::Ordering::SeqCst) {
                                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                            }
                        }
                    },
                )
                .await;

                let recovery_wait_ms = autolaunch_start.elapsed().as_millis();
                let preferred_type = if prefer_conda { "conda" } else { "uv" };
                if recovery_timeout.is_err() {
                    log::info!(
                        "[autolaunch] Recovery timeout after {}ms (preferred: {}), UV: {}, Conda: {}",
                        recovery_wait_ms,
                        preferred_type,
                        uv_recovery_for_autolaunch.load(std::sync::atomic::Ordering::SeqCst),
                        conda_recovery_for_autolaunch.load(std::sync::atomic::Ordering::SeqCst)
                    );
                } else {
                    log::info!(
                        "[autolaunch] {} recovery complete in {}ms, proceeding with auto-launch",
                        preferred_type,
                        recovery_wait_ms
                    );
                }

                // Log pool status before attempting to get prewarmed env
                let uv_status = pool_for_autolaunch.lock().await.status();
                let conda_status = conda_pool_for_autolaunch.lock().await.status();
                log::info!(
                    "[autolaunch] Pool status - UV: {}/{} ready ({} creating), Conda: {}/{} ready ({} creating)",
                    uv_status.available, uv_status.target, uv_status.creating,
                    conda_status.available, conda_status.target, conda_status.creating
                );

                // Get runtime and verify trust before launching
                let (runtime, trust_status) = {
                    match notebook_for_autolaunch.lock() {
                        Ok(state) => {
                            let rt = state.get_runtime();
                            let trust_result =
                                trust::verify_notebook_trust(&state.notebook.metadata.additional);
                            (rt, trust_result)
                        }
                        Err(_) => return,
                    }
                };

                // Only auto-launch for trusted notebooks or those with no dependencies
                // Untrusted notebooks must wait for user approval via frontend dialog
                let trust_info = match trust_status {
                    Ok(info) => info,
                    Err(e) => {
                        log::warn!("Trust verification failed, skipping auto-launch: {}", e);
                        return;
                    }
                };

                match trust_info.status {
                    trust::TrustStatus::Trusted | trust::TrustStatus::NoDependencies => {
                        // Safe to auto-launch
                    }
                    trust::TrustStatus::Untrusted | trust::TrustStatus::SignatureInvalid => {
                        log::info!(
                            "Notebook not trusted, skipping auto-launch (will prompt user)"
                        );
                        return;
                    }
                }

                // Set auto-launch flag so frontend can query state
                auto_launch_flag.store(true, Ordering::SeqCst);

                // Emit lifecycle event so frontend can show "Starting" status
                let runtime_str = runtime.to_string();
                let lifecycle_event = KernelLifecycleEvent {
                    state: "launching".to_string(),
                    runtime: runtime_str.clone(),
                    env_source: None,
                    error_message: None,
                };
                let _ = app_for_autolaunch.emit("kernel:lifecycle", &lifecycle_event);

                let (env_source, error_msg) = match &runtime {
                    Runtime::Python => {
                        match start_default_python_kernel_impl(
                            app_for_autolaunch.clone(),
                            &notebook_for_autolaunch,
                            &kernel_for_autolaunch,
                            &pool_for_autolaunch,
                            &conda_pool_for_autolaunch,
                        )
                        .await
                        {
                            Ok(source) => (Some(source), None),
                            Err(e) => {
                                log::error!("Auto-launch kernel failed: {}", e);
                                (None, Some(e.to_string()))
                            }
                        }
                    }
                    Runtime::Deno => {
                        match start_deno_kernel_impl(
                            app_for_autolaunch.clone(),
                            &notebook_for_autolaunch,
                            &kernel_for_autolaunch,
                        )
                        .await
                        {
                            Ok(()) => (Some("deno".to_string()), None),
                            Err(e) => {
                                log::error!("Auto-launch Deno kernel failed: {}", e);
                                (None, Some(e.to_string()))
                            }
                        }
                    }
                    Runtime::Other(s) => {
                        let msg = format!("No kernel available for runtime: {s}");
                        log::error!("{}", msg);
                        (None, Some(msg))
                    }
                };

                if let Some(source) = env_source {
                    let ready_event = KernelLifecycleEvent {
                        state: "ready".to_string(),
                        runtime: runtime_str,
                        env_source: Some(source),
                        error_message: None,
                    };
                    let _ = app_for_autolaunch.emit("kernel:lifecycle", &ready_event);
                } else {
                    let error_event = KernelLifecycleEvent {
                        state: "error".to_string(),
                        runtime: runtime_str,
                        env_source: None,
                        error_message: error_msg,
                    };
                    let _ = app_for_autolaunch.emit("kernel:lifecycle", &error_event);
                }

                // Clear auto-launch flag when done
                auto_launch_flag.store(false, Ordering::SeqCst);
            });

            Ok(())
        })
        .on_menu_event(|app, event| {
            match event.id().as_ref() {
                crate::menu::MENU_NEW_NOTEBOOK => {
                    // Spawn notebook using the user's default runtime preference
                    let runtime = settings::load_settings().default_runtime;
                    spawn_new_notebook(runtime);
                }
                crate::menu::MENU_NEW_PYTHON_NOTEBOOK => {
                    spawn_new_notebook(Runtime::Python);
                }
                crate::menu::MENU_NEW_DENO_NOTEBOOK => {
                    spawn_new_notebook(Runtime::Deno);
                }
                crate::menu::MENU_OPEN => {
                    // Emit event to frontend to trigger open dialog
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.emit("menu:open", ());
                    }
                }
                crate::menu::MENU_SAVE => {
                    // Emit event to frontend to trigger save
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.emit("menu:save", ());
                    }
                }
                crate::menu::MENU_CLONE_NOTEBOOK => {
                    // Emit event to frontend to trigger clone
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.emit("menu:clone", ());
                    }
                }
                crate::menu::MENU_ZOOM_IN => {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.emit("menu:zoom-in", ());
                    }
                }
                crate::menu::MENU_ZOOM_OUT => {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.emit("menu:zoom-out", ());
                    }
                }
                crate::menu::MENU_ZOOM_RESET => {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.emit("menu:zoom-reset", ());
                    }
                }
                crate::menu::MENU_RUN_ALL_CELLS => {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.emit("menu:run-all", ());
                    }
                }
                crate::menu::MENU_RESTART_AND_RUN_ALL => {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.emit("menu:restart-and-run-all", ());
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
                _ => {}
            }
        })
        .on_window_event(move |_window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                // Shutdown kernel when window is closed
                let kernel = kernel_for_window_event.clone();
                tauri::async_runtime::block_on(async {
                    let mut k = kernel.lock().await;
                    if let Err(e) = k.shutdown().await {
                        log::error!("Failed to shutdown kernel on window close: {}", e);
                    }
                });
            }
        })
        .build(tauri::generate_context!())
        .map_err(|e| anyhow::anyhow!("Tauri build error: {}", e))?;

    app.run(move |_app_handle, event| {
        // Handle file associations (macOS only)
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        if let RunEvent::Opened { urls } = &event {
            for url in urls {
                let path = match url.scheme() {
                    "file" => url.to_file_path().ok(),
                    _ => None,
                };
                let Some(path) = path else { continue };
                if path.extension().and_then(|e| e.to_str()) != Some("ipynb") {
                    continue;
                }

                // If the current window has no notebook loaded, open it here.
                // Otherwise spawn a new process.
                let has_path = notebook_for_open
                    .lock()
                    .map(|s| s.path.is_some())
                    .unwrap_or(false);

                if !has_path {
                    // Load into the current window
                    match std::fs::read_to_string(&path) {
                        Ok(content) => match nbformat::parse_notebook(&content) {
                            Ok(nb) => {
                                let nb_v4 = match nb {
                                    nbformat::Notebook::V4(nb) => nb,
                                    nbformat::Notebook::Legacy(legacy) => {
                                        match nbformat::upgrade_legacy_notebook(legacy) {
                                            Ok(nb) => nb,
                                            Err(e) => {
                                                log::error!("Failed to upgrade notebook: {}", e);
                                                continue;
                                            }
                                        }
                                    }
                                };
                                let new_state = NotebookState::from_notebook(nb_v4, path.clone());
                                if let Ok(mut state) = notebook_for_open.lock() {
                                    *state = new_state;
                                }
                                // Update window title and tell frontend to reload
                                if let Some(window) = _app_handle.get_webview_window("main") {
                                    let title = path
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("Untitled.ipynb");
                                    let _ = window.set_title(title);
                                    let _ = window.emit("notebook:file-opened", ());
                                }
                            }
                            Err(e) => log::error!("Failed to parse notebook: {}", e),
                        },
                        Err(e) => log::error!("Failed to read notebook file: {}", e),
                    }
                } else {
                    // Already have a notebook open — spawn a new process
                    if let Ok(exe) = std::env::current_exe() {
                        let _ = std::process::Command::new(exe).arg(&path).spawn();
                    }
                }
            }
        }

        // Handle app exit
        if let RunEvent::Exit = event {
            // Shutdown kernel when app exits
            let kernel = kernel_for_exit.clone();
            tauri::async_runtime::block_on(async {
                let mut k = kernel.lock().await;
                if let Err(e) = k.shutdown().await {
                    log::error!("Failed to shutdown kernel on app exit: {}", e);
                }
            });
        }
    });

    Ok(())
}
