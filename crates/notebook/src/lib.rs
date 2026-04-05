pub mod cli_install;
pub mod conda_env;
pub mod deno_env;
pub mod environment_yml;
pub mod mcpb_install;
pub mod menu;

pub mod pixi;
pub mod pyproject;
pub mod session;
pub mod settings;
pub mod shell_env;
pub mod trust;
pub mod typosquat;
pub mod uv_env;

extern crate runtimed_client as runtimed;
pub use runtimed::runtime::Runtime;

use notebook_protocol::protocol::{
    CompletionItem, HistoryEntry, NotebookRequest, NotebookResponse,
};
use notebook_sync::RelayHandle;

use log::{debug, info, warn};
use serde::Serialize;
use std::collections::HashMap;
use std::ffi::OsStr;

/// Shared notebook sync handle for cross-window state synchronization.
/// The Option allows graceful fallback when daemon is unavailable.
/// Uses the split handle pattern - the handle is clonable and doesn't block.
type SharedNotebookSync = Arc<tokio::sync::Mutex<Option<RelayHandle>>>;

#[derive(Clone)]
struct WindowNotebookContext {
    notebook_sync: SharedNotebookSync,
    /// Generation counter to prevent stale broadcast tasks from clobbering new connections.
    /// Incremented each time a sync init function is called (open, create, or reconnect).
    sync_generation: Arc<AtomicU64>,
    /// Notebook file path — authoritative for path reads (has_notebook_path, etc.)
    path: Arc<Mutex<Option<PathBuf>>>,
    /// Working directory for untitled notebooks (project file detection).
    working_dir: Option<PathBuf>,
    /// Dirty flag — tracks unsaved changes (authoritative source).
    dirty: Arc<AtomicBool>,
    /// Notebook ID for daemon sync — derived from path (saved) or env_id (untitled).
    /// Updated on save_notebook_as when path changes.
    notebook_id: Arc<Mutex<String>>,
    /// Runtime type for this notebook (Python or Deno).
    /// Used by session save so it doesn't need to query the daemon.
    runtime: Runtime,
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
        let label = label.into();
        let mut contexts = self.contexts.lock().map_err(|e| e.to_string())?;
        if contexts.contains_key(&label) {
            return Err(format!("Context already exists for window '{}'", label));
        }
        let has_path = context.path.lock().is_ok_and(|p| p.is_some());
        contexts.insert(label.clone(), context);
        log::info!(
            "[registry] Registered context for '{}' (has_path={}, total={})",
            label,
            has_path,
            contexts.len()
        );
        Ok(())
    }

    /// Remove registry entries whose windows no longer exist.
    fn prune_stale_entries(&self, app: &tauri::AppHandle) {
        self.prune_where(|label| app.get_webview_window(label).is_none());
    }

    /// Remove registry entries where the predicate returns true.
    fn prune_where(&self, is_stale: impl Fn(&str) -> bool) {
        let mut contexts = match self.contexts.lock() {
            Ok(c) => c,
            Err(e) => {
                log::warn!("[registry] Failed to lock contexts for pruning: {}", e);
                return;
            }
        };
        let stale: Vec<String> = contexts
            .keys()
            .filter(|label| is_stale(label))
            .cloned()
            .collect();
        if stale.is_empty() {
            log::debug!(
                "[registry] Prune found no stale entries ({} total)",
                contexts.len()
            );
        }
        for label in stale {
            contexts.remove(&label);
            log::info!(
                "[registry] Pruned stale entry '{}' ({} remaining)",
                label,
                contexts.len()
            );
        }
    }

    fn get(&self, label: &str) -> Result<WindowNotebookContext, String> {
        let contexts = self.contexts.lock().map_err(|e| e.to_string())?;
        contexts.get(label).cloned().ok_or_else(|| {
            format!(
                "No notebook context for window '{}' (registry has {} entries)",
                label,
                contexts.len()
            )
        })
    }

    /// Find the first window label whose stored path matches `target`.
    #[cfg(target_os = "macos")]
    fn find_label_by_path(&self, target: &Path) -> Option<String> {
        let contexts = self.contexts.lock().ok()?;
        for (label, ctx) in contexts.iter() {
            if let Ok(guard) = ctx.path.lock() {
                if guard.as_deref() == Some(target) {
                    return Some(label.clone());
                }
            }
        }
        None
    }

    /// Find the first live window that has no file path (untitled/empty notebook).
    #[cfg(target_os = "macos")]
    fn find_empty_window_label(&self, app: &tauri::AppHandle) -> Option<String> {
        let contexts = self.contexts.lock().ok()?;
        for (label, ctx) in contexts.iter() {
            if app.get_webview_window(label).is_some() {
                if let Ok(guard) = ctx.path.lock() {
                    if guard.is_none() {
                        log::info!("[registry] find_empty_window_label: found '{}'", label);
                        return Some(label.clone());
                    }
                }
            }
        }
        log::debug!("[registry] find_empty_window_label: no empty window found");
        None
    }
}

/// Newtype wrapper for reconnect-in-progress flag (distinguishes from other AtomicBool states).
struct ReconnectInProgress(Arc<AtomicBool>);

/// Newtype wrapper for daemon-restart-in-progress flag.
/// Prevents multiple windows from attempting to restart the daemon simultaneously.
struct DaemonRestartInProgress(Arc<AtomicBool>);

/// Tracks the last daemon progress status for UI queries.
/// This allows the frontend to check status on mount (in case events were missed).
struct DaemonStatusState(Arc<Mutex<Option<runtimed::client::DaemonProgress>>>);

/// Per-window sync readiness gate.
///
/// The Tauri relay task buffers daemon frames in the mpsc channel and waits
/// for the frontend to signal readiness before emitting `notebook:frame` events.
/// This prevents frame loss when the JS `SyncEngine` hasn't subscribed yet
/// (race between relay start and `engine.start()` + `webview.listen()`).
///
/// On the first connection the relay blocks until the JS calls `notify_sync_ready`.
/// On reconnection the flag is already `true`, so frames flow immediately.
#[derive(Clone, Default)]
struct SyncReadyState {
    senders: Arc<Mutex<HashMap<String, tokio::sync::watch::Sender<bool>>>>,
}

impl SyncReadyState {
    /// Mark a window's relay as ready to emit frames.
    ///
    /// If the relay hasn't subscribed yet (JS signaled before the Rust
    /// connection finished), creates the entry pre-seeded to `true` so
    /// the later `subscribe()` picks it up immediately.
    fn set_ready(&self, label: &str) {
        let mut senders = match self.senders.lock() {
            Ok(s) => s,
            Err(e) => e.into_inner(),
        };
        match senders.get(label) {
            Some(tx) => {
                let _ = tx.send(true);
            }
            None => {
                // Pre-seed: JS signaled before the relay subscribed.
                senders.insert(label.to_string(), tokio::sync::watch::channel(true).0);
            }
        }
    }

    /// Get a receiver for the readiness flag, creating the entry if needed.
    ///
    /// The receiver starts at the sender's current value: `false` on first
    /// connection, `true` on reconnection (since the JS listener persists).
    fn subscribe(&self, label: &str) -> tokio::sync::watch::Receiver<bool> {
        let mut senders = match self.senders.lock() {
            Ok(s) => s,
            Err(e) => e.into_inner(),
        };
        let tx = senders
            .entry(label.to_string())
            .or_insert_with(|| tokio::sync::watch::channel(false).0);
        tx.subscribe()
    }
}

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};
use tauri::{RunEvent, WindowEvent};

/// Payload emitted with the `daemon:ready` event after daemon-owned notebook loading.
/// Carries notebook identity and trust status so the frontend can show loading state (#599)
/// and trust prompts without additional round-trips.
#[derive(Clone, Serialize)]
struct DaemonReadyPayload {
    notebook_id: String,
    cell_count: usize,
    needs_trust_approval: bool,
    /// Runtime hint so the frontend can show the correct UI before metadata syncs.
    /// Only set for Create (where we know the exact runtime); None for Open
    /// (where the actual runtime is determined from the file's metadata).
    runtime: Option<String>,
}

/// How to connect a new window to the daemon.
enum OpenMode {
    /// Open an existing notebook file. Daemon loads from disk.
    Open { path: PathBuf },
    /// Create a new empty notebook, or restore an untitled notebook from a previous session.
    ///
    /// If `notebook_id` is provided, the daemon reuses the existing room (and its persisted
    /// Automerge doc) instead of generating a new UUID. This handles session restore for
    /// untitled notebooks that were never saved to disk.
    Create {
        runtime: String,
        working_dir: Option<PathBuf>,
        notebook_id: Option<String>,
    },
}

/// Git information for debug banner display.
#[derive(Serialize)]
struct GitInfo {
    branch: String,
    commit: String,
    description: Option<String>,
}

/// Status of a notebook for the upgrade screen.
#[derive(Debug, Clone, Serialize)]
struct UpgradeNotebookStatus {
    window_label: String,
    notebook_id: String,
    display_name: String,
    kernel_status: Option<String>,
    is_dirty: bool,
}

/// Progress events emitted during upgrade.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
enum UpgradeProgress {
    SavingNotebooks,
    StoppingRuntimes,
    ClosingWindows,
    UpgradingDaemon,
    Ready,
    Failed { error: String },
}

fn notebook_sync_for_window(
    window: &tauri::Window,
    registry: &WindowNotebookRegistry,
) -> Result<SharedNotebookSync, String> {
    Ok(registry.get(window.label())?.notebook_sync)
}

fn sync_generation_for_window(
    window: &tauri::Window,
    registry: &WindowNotebookRegistry,
) -> Result<Arc<AtomicU64>, String> {
    Ok(registry.get(window.label())?.sync_generation)
}

fn path_for_window(
    window: &tauri::Window,
    registry: &WindowNotebookRegistry,
) -> Result<Arc<Mutex<Option<PathBuf>>>, String> {
    Ok(registry.get(window.label())?.path)
}

fn working_dir_for_window(
    window: &tauri::Window,
    registry: &WindowNotebookRegistry,
) -> Result<Option<PathBuf>, String> {
    Ok(registry.get(window.label())?.working_dir.clone())
}

fn dirty_for_window(
    window: &tauri::Window,
    registry: &WindowNotebookRegistry,
) -> Result<Arc<AtomicBool>, String> {
    Ok(registry.get(window.label())?.dirty.clone())
}

fn notebook_id_for_window(
    window: &tauri::Window,
    registry: &WindowNotebookRegistry,
) -> Result<Arc<Mutex<String>>, String> {
    Ok(registry.get(window.label())?.notebook_id.clone())
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

/// Read the notebook metadata from the daemon's canonical Automerge doc.
/// Returns the deserialized NotebookMetadataSnapshot, or None if not available.
async fn get_metadata_snapshot(
    handle: &RelayHandle,
) -> Option<notebook_doc::metadata::NotebookMetadataSnapshot> {
    match handle
        .send_request(NotebookRequest::GetMetadataSnapshot {})
        .await
    {
        Ok(NotebookResponse::MetadataSnapshot {
            snapshot: Some(json),
        }) => serde_json::from_str(&json).ok(),
        _ => None,
    }
}

/// Write a NotebookMetadataSnapshot to the daemon's canonical Automerge doc.
async fn set_metadata_snapshot(
    handle: &RelayHandle,
    snapshot: &notebook_doc::metadata::NotebookMetadataSnapshot,
) -> Result<(), String> {
    let snapshot_json =
        serde_json::to_string(snapshot).map_err(|e| format!("serialize metadata: {}", e))?;
    match handle
        .send_request(NotebookRequest::SetMetadataSnapshot {
            snapshot: snapshot_json,
        })
        .await
    {
        Ok(NotebookResponse::MetadataSet {}) => Ok(()),
        Ok(NotebookResponse::Error { error }) => Err(error),
        Ok(other) => Err(format!("Unexpected response: {:?}", other)),
        Err(e) => Err(format!("set_metadata request failed: {}", e)),
    }
}

/// Read the metadata `additional` fields from the daemon's Automerge doc.
/// Returns a HashMap with the `runt` field as a JSON value for trust verification.
async fn get_raw_metadata_additional(
    handle: &RelayHandle,
) -> Option<HashMap<String, serde_json::Value>> {
    let snapshot = get_metadata_snapshot(handle).await?;
    let runt_value = serde_json::to_value(&snapshot.runt).ok()?;
    let mut additional = HashMap::new();
    additional.insert("runt".to_string(), runt_value);
    Some(additional)
}

/// Write trust fields into the daemon's metadata.
async fn set_raw_trust_in_metadata(
    handle: &RelayHandle,
    signature: &str,
    timestamp: &str,
) -> Result<(), String> {
    let mut snapshot = get_metadata_snapshot(handle)
        .await
        .ok_or("No metadata in Automerge doc")?;

    snapshot.runt.trust_signature = Some(signature.to_string());
    snapshot.runt.trust_timestamp = Some(timestamp.to_string());

    set_metadata_snapshot(handle, &snapshot).await
}

/// Reconstruct an nbformat Metadata from a NotebookMetadataSnapshot.
/// Used to bridge sync-handle metadata to extraction functions that expect nbformat types.
fn metadata_from_snapshot(
    snapshot: &notebook_doc::metadata::NotebookMetadataSnapshot,
) -> nbformat::v4::Metadata {
    let mut metadata = nbformat::v4::Metadata {
        kernelspec: snapshot
            .kernelspec
            .as_ref()
            .map(|ks| nbformat::v4::KernelSpec {
                name: ks.name.clone(),
                display_name: ks.display_name.clone(),
                language: ks.language.clone(),
                additional: std::collections::HashMap::new(),
            }),
        language_info: snapshot
            .language_info
            .as_ref()
            .map(|li| nbformat::v4::LanguageInfo {
                name: li.name.clone(),
                version: li.version.clone(),
                codemirror_mode: None,
                additional: std::collections::HashMap::new(),
            }),
        authors: None,
        additional: std::collections::HashMap::new(),
    };
    // Serialize runt back to additional
    if let Ok(runt_value) = serde_json::to_value(&snapshot.runt) {
        metadata.additional.insert("runt".to_string(), runt_value);
    }
    metadata
}

/// Helper to create a default empty NotebookMetadataSnapshot.
fn default_metadata_snapshot() -> notebook_doc::metadata::NotebookMetadataSnapshot {
    notebook_doc::metadata::NotebookMetadataSnapshot {
        kernelspec: None,
        language_info: None,
        runt: notebook_doc::metadata::RuntMetadata {
            schema_version: "1".to_string(),
            env_id: None,
            uv: None,
            conda: None,
            pixi: None,
            deno: None,
            trust_signature: None,
            trust_timestamp: None,
            extra: std::collections::BTreeMap::new(),
        },
    }
}

/// Convert nbformat metadata into a `NotebookMetadataSnapshot`.
///
/// Used by the import commands (pyproject, pixi) that build nbformat metadata
/// locally then need to push it to the daemon as a typed snapshot.
fn snapshot_from_nbformat(
    metadata: &nbformat::v4::Metadata,
) -> notebook_doc::metadata::NotebookMetadataSnapshot {
    use notebook_doc::metadata::*;

    let kernelspec = metadata.kernelspec.as_ref().map(|ks| KernelspecSnapshot {
        name: ks.name.clone(),
        display_name: ks.display_name.clone(),
        language: ks.language.clone(),
    });

    let language_info = metadata
        .language_info
        .as_ref()
        .map(|li| LanguageInfoSnapshot {
            name: li.name.clone(),
            version: li.version.clone(),
        });

    let convert_legacy_deno = |dd: crate::deno_env::DenoDependencies| DenoMetadata {
        permissions: dd.permissions,
        import_map: dd.import_map,
        config: dd.config,
        flexible_npm_imports: if dd.flexible_npm_imports {
            None
        } else {
            Some(false)
        },
    };

    let runt = if let Some(runt_value) = metadata.additional.get("runt") {
        let mut runt_meta = serde_json::from_value::<RuntMetadata>(runt_value.clone())
            .unwrap_or_else(|_| RuntMetadata {
                schema_version: "1".to_string(),
                env_id: None,
                uv: None,
                conda: None,
                pixi: None,
                deno: None,
                trust_signature: None,
                trust_timestamp: None,
                extra: std::collections::BTreeMap::new(),
            });

        if runt_meta.deno.is_none() {
            if let Some(deno_value) = metadata.additional.get("deno") {
                if let Ok(dd) =
                    serde_json::from_value::<crate::deno_env::DenoDependencies>(deno_value.clone())
                {
                    runt_meta.deno = Some(convert_legacy_deno(dd));
                }
            }
        }

        runt_meta
    } else {
        let uv = metadata
            .additional
            .get("uv")
            .and_then(|v| serde_json::from_value::<UvInlineMetadata>(v.clone()).ok());
        let conda = metadata
            .additional
            .get("conda")
            .and_then(|v| serde_json::from_value::<CondaInlineMetadata>(v.clone()).ok());
        let deno = metadata
            .additional
            .get("deno")
            .and_then(|v| {
                serde_json::from_value::<crate::deno_env::DenoDependencies>(v.clone()).ok()
            })
            .map(convert_legacy_deno);

        RuntMetadata {
            schema_version: "1".to_string(),
            env_id: None,
            uv,
            conda,
            pixi: None,
            deno,
            trust_signature: None,
            trust_timestamp: None,
            extra: std::collections::BTreeMap::new(),
        }
    };

    NotebookMetadataSnapshot {
        kernelspec,
        language_info,
        runt,
    }
}

/// Connect to the daemon by opening an existing notebook file.
///
/// The daemon loads the file, derives notebook_id, creates the room, and populates
/// the Automerge doc. Returns after sync is established and `daemon:ready` is emitted.
async fn initialize_notebook_sync_open(
    window: tauri::WebviewWindow,
    path: PathBuf,
    notebook_sync: SharedNotebookSync,
    sync_generation: Arc<AtomicU64>,
    notebook_id: Arc<Mutex<String>>,
) -> Result<(), String> {
    let current_generation = sync_generation.fetch_add(1, Ordering::SeqCst) + 1;

    let socket_path = runt_workspace::default_socket_path();
    info!(
        "[notebook-sync] Opening notebook via daemon: {} ({})",
        path.display(),
        socket_path.display(),
    );

    let (frame_tx, raw_frame_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

    let result = notebook_sync::connect::connect_open_relay(socket_path, path, frame_tx)
        .await
        .map_err(|e| format!("sync connect (open): {}", e))?;

    let handle = result.handle;
    let info = result.info;

    info!(
        "[notebook-sync] Daemon opened notebook: id={}, cells={}, trust_approval={}",
        info.notebook_id, info.cell_count, info.needs_trust_approval,
    );

    // Update notebook_id with the daemon's canonical ID
    if let Ok(mut id) = notebook_id.lock() {
        *id = info.notebook_id.clone();
    }

    let ready_payload = DaemonReadyPayload {
        notebook_id: info.notebook_id.clone(),
        cell_count: info.cell_count,
        needs_trust_approval: info.needs_trust_approval,
        runtime: None,
    };

    setup_sync_receivers(
        window,
        info.notebook_id,
        handle,
        raw_frame_rx,
        notebook_sync,
        sync_generation,
        current_generation,
        ready_payload,
    )
    .await
}

/// Connect to the daemon by creating a new empty notebook.
///
/// The daemon creates an empty notebook with one code cell, generates a notebook_id
/// (UUID/env_id), and returns it. Returns after sync is established and `daemon:ready` is emitted.
async fn initialize_notebook_sync_create(
    window: tauri::WebviewWindow,
    runtime: String,
    working_dir: Option<PathBuf>,
    notebook_id_hint: Option<String>,
    notebook_sync: SharedNotebookSync,
    sync_generation: Arc<AtomicU64>,
    notebook_id: Arc<Mutex<String>>,
) -> Result<(), String> {
    let current_generation = sync_generation.fetch_add(1, Ordering::SeqCst) + 1;

    let socket_path = runt_workspace::default_socket_path();
    info!(
        "[notebook-sync] Creating notebook via daemon: runtime={}, working_dir={:?}, notebook_id_hint={:?} ({})",
        runtime,
        working_dir,
        notebook_id_hint,
        socket_path.display(),
    );

    let (frame_tx, raw_frame_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

    let result = notebook_sync::connect::connect_create_relay(
        socket_path,
        &runtime,
        working_dir,
        notebook_id_hint,
        frame_tx,
    )
    .await
    .map_err(|e| format!("sync connect (create): {}", e))?;

    let handle = result.handle;
    let info = result.info;

    info!(
        "[notebook-sync] Daemon created notebook: id={}, cells={}",
        info.notebook_id, info.cell_count,
    );

    // Update notebook_id with the daemon's generated UUID
    if let Ok(mut id) = notebook_id.lock() {
        *id = info.notebook_id.clone();
    }

    let ready_payload = DaemonReadyPayload {
        notebook_id: info.notebook_id.clone(),
        cell_count: info.cell_count,
        needs_trust_approval: info.needs_trust_approval,
        runtime: Some(runtime),
    };

    setup_sync_receivers(
        window,
        info.notebook_id,
        handle,
        raw_frame_rx,
        notebook_sync,
        sync_generation,
        current_generation,
        ready_payload,
    )
    .await
}

/// Store the sync handle and spawn the unified frame relay for an established daemon connection.
///
/// This is the common tail of `initialize_notebook_sync_open` and `_create`.
/// It stores the handle, spawns a single relay task that forwards all typed
/// frames (AutomergeSync, Broadcast, Presence) to the frontend via the
/// `notebook:frame` event, and emits `daemon:ready` with the connection payload.
///
/// Note: No SyncUpdate receiver task is spawned — in pipe mode the relay forwards
/// raw Automerge bytes directly, and the frontend WASM drives metadata updates
/// via `useSyncExternalStore`.
#[allow(clippy::too_many_arguments)]
async fn setup_sync_receivers(
    window: tauri::WebviewWindow,
    notebook_id: String,
    handle: RelayHandle,
    mut raw_frame_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    notebook_sync: SharedNotebookSync,
    sync_generation: Arc<AtomicU64>,
    current_generation: u64,
    ready_payload: DaemonReadyPayload,
) -> Result<(), String> {
    // Store the handle for commands to use
    *notebook_sync.lock().await = Some(handle);
    info!(
        "[notebook-sync] Handle stored for {} (gen {})",
        notebook_id, current_generation,
    );

    // Spawn unified frame relay task — forwards all typed frames (AutomergeSync,
    // Broadcast, Presence) to the frontend as raw bytes via one event.
    // On disconnect, conditionally clears the handle using the generation counter
    // to avoid clobbering a newer connection's handle.
    let window_for_ready = window.clone();
    let notebook_sync_for_disconnect = notebook_sync.clone();
    let notebook_id_for_relay = notebook_id.clone();
    let sync_generation_for_cleanup = sync_generation.clone();

    // Subscribe to the per-window readiness gate. On the first connection this
    // starts at `false` (relay waits until JS calls `notify_sync_ready`). On
    // reconnection the flag is already `true` so the relay proceeds immediately.
    let mut ready_rx = window
        .app_handle()
        .state::<SyncReadyState>()
        .subscribe(window.label());

    tokio::spawn(async move {
        // Wait for the frontend SyncEngine to signal readiness. Daemon frames
        // buffer in `raw_frame_rx` during this wait — the mpsc channel is
        // unbounded, so nothing is lost.
        if !*ready_rx.borrow() {
            info!(
                "[notebook-sync] Waiting for frontend ready before emitting frames (gen {})",
                current_generation,
            );
            match tokio::time::timeout(
                std::time::Duration::from_secs(30),
                ready_rx.wait_for(|&ready| ready),
            )
            .await
            {
                Ok(Ok(_)) => {
                    info!(
                        "[notebook-sync] Frontend signaled ready (gen {})",
                        current_generation,
                    );
                }
                _ => {
                    warn!(
                        "[notebook-sync] Frontend ready timeout after 30s (gen {}) — proceeding anyway",
                        current_generation,
                    );
                }
            }
        }

        while let Some(frame_bytes) = raw_frame_rx.recv().await {
            // Stop forwarding if a newer connection has replaced this one.
            // Without this check, frames from the old room can interleave
            // with the new connection's frames on the notebook:frame event
            // channel, causing the frontend to feed them to the wrong WASM
            // handle (stale Automerge document).
            if sync_generation_for_cleanup.load(Ordering::SeqCst) != current_generation {
                info!(
                    "[notebook-sync] Stale relay for {} (gen {} superseded) — stopping frame emission",
                    notebook_id_for_relay, current_generation,
                );
                break;
            }
            if let Err(e) =
                emit_to_label::<_, _, _>(&window, window.label(), "notebook:frame", &frame_bytes)
            {
                warn!("[notebook-sync] Failed to emit notebook:frame: {}", e);
            }
        }
        warn!(
            "[notebook-sync] Frame relay ended for {} (gen {}) — daemon disconnected",
            notebook_id_for_relay, current_generation,
        );

        let current_gen = sync_generation_for_cleanup.load(Ordering::SeqCst);
        if current_gen == current_generation {
            info!(
                "[notebook-sync] Clearing handle for {} (gen {})",
                notebook_id_for_relay, current_generation,
            );
            *notebook_sync_for_disconnect.lock().await = None;
            if let Err(e) =
                emit_to_label::<_, _, _>(&window, window.label(), "daemon:disconnected", ())
            {
                warn!("[notebook-sync] Failed to emit daemon:disconnected: {}", e);
            }
        } else {
            info!(
                "[notebook-sync] Skipping cleanup for {} (gen {} != {})",
                notebook_id_for_relay, current_generation, current_gen,
            );
        }
    });

    info!(
        "[notebook-sync] Sync receivers established for {}",
        notebook_id,
    );

    // Emit daemon:ready with connection info so frontend can show loading state / trust prompt
    if let Err(e) = emit_to_label::<_, _, _>(
        &window_for_ready,
        window_for_ready.label(),
        "daemon:ready",
        &ready_payload,
    ) {
        warn!("[notebook-sync] Failed to emit daemon:ready: {}", e);
    }

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{next_available_sample_path, reopen_action, ReopenAction};
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

    #[test]
    fn reopen_action_restores_hidden_windows_before_spawning() {
        assert_eq!(reopen_action(false, 1), Some(ReopenAction::RestoreWindow));
    }

    #[test]
    fn reopen_action_spawns_when_all_windows_are_closed() {
        assert_eq!(reopen_action(false, 0), Some(ReopenAction::SpawnNotebook));
    }

    #[test]
    fn reopen_action_ignores_reopen_events_when_a_window_is_visible() {
        assert_eq!(reopen_action(true, 1), None);
    }
}

/// Get the version string of the bundled daemon.
/// Format: "{CARGO_PKG_VERSION}+{GIT_COMMIT}" e.g., "1.4.1+a1b2c3d"
fn bundled_daemon_version() -> String {
    format!("{}+{}", env!("CARGO_PKG_VERSION"), env!("GIT_COMMIT"))
}

/// Extract the commit hash from a version string.
/// Version format: "X.Y.Z+COMMIT" -> returns "COMMIT"
fn extract_commit_hash(version: &str) -> Option<&str> {
    version.split('+').nth(1)
}

/// Upgrade the daemon via sidecar when version mismatch detected.
///
/// Runs `runtimed install` which handles: stop old → copy binary → start new.
async fn upgrade_daemon_via_sidecar<F>(
    app: &tauri::AppHandle,
    on_progress: F,
) -> Result<String, String>
where
    F: Fn(runtimed::client::DaemonProgress) + Clone + Send + 'static,
{
    use runtimed::client::DaemonProgress;
    use tauri_plugin_shell::{process::CommandEvent, ShellExt};

    let bundled = bundled_daemon_version();
    log::info!("[startup] Upgrading daemon to bundled version: {}", bundled);
    on_progress(DaemonProgress::Installing); // Reuse "installing" state for upgrade

    // "runtimed install" handles: stop old → copy binary → start new
    // The sidecar resolves to the runtimed binary bundled in the app.
    // After a Tauri downloadAndInstall(), this should be the NEW binary.
    let (mut rx, _child) = app
        .shell()
        .sidecar("runtimed")
        .map_err(|e| format!("Failed to create sidecar: {}", e))?
        .args(["install"])
        .spawn()
        .map_err(|e| format!("Failed to spawn sidecar: {}", e))?;

    // Collect output for logging
    let mut exit_code = None;
    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(line) => {
                log::info!(
                    "[runtimed upgrade] {}",
                    String::from_utf8_lossy(&line).trim()
                );
            }
            CommandEvent::Stderr(line) => {
                log::warn!(
                    "[runtimed upgrade] {}",
                    String::from_utf8_lossy(&line).trim()
                );
            }
            CommandEvent::Terminated(status) => {
                exit_code = status.code;
            }
            _ => {}
        }
    }

    if exit_code != Some(0) {
        let error = format!("Daemon upgrade failed with code {:?}", exit_code);
        log::error!("[startup] {}", error);
        on_progress(DaemonProgress::Failed {
            error: error.clone(),
            guidance: format!(
                "Try running: {} install",
                runt_workspace::daemon_binary_basename()
            ),
        });
        return Err(error);
    }

    // Wait for upgraded daemon to be ready
    log::info!("[startup] Sidecar install exited successfully, waiting for daemon to be ready...");
    on_progress(DaemonProgress::Starting);

    let client = runtimed::client::PoolClient::default();
    let max_attempts = 40;
    for attempt in 1..=max_attempts {
        on_progress(DaemonProgress::WaitingForReady {
            attempt,
            max_attempts,
        });

        if client.ping().await.is_ok() {
            let endpoint = runt_workspace::default_socket_path()
                .to_string_lossy()
                .to_string();

            // Verify the running daemon version matches what we intended to install
            if let Some(info) = runtimed::singleton::get_running_daemon_info() {
                let running_commit = extract_commit_hash(&info.version);
                let bundled_commit = extract_commit_hash(&bundled);
                if running_commit == bundled_commit {
                    log::info!(
                        "[startup] Upgraded daemon version confirmed: {} (attempt {})",
                        info.version,
                        attempt
                    );
                } else {
                    log::warn!(
                        "[startup] Daemon version mismatch after upgrade! running={}, bundled={}",
                        info.version,
                        bundled
                    );
                }
            }

            on_progress(DaemonProgress::Ready {
                endpoint: endpoint.clone(),
            });
            return Ok(endpoint);
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    let error = "Upgraded daemon did not become ready within timeout".to_string();
    log::error!("[startup] {}", error);
    on_progress(DaemonProgress::Failed {
        error: error.clone(),
        guidance: format!(
            "Check daemon logs: {} daemon logs",
            runt_workspace::cli_command_name()
        ),
    });
    Err(error)
}

/// Install/upgrade the daemon in preparation for app restart after update.
///
/// Called by the frontend before `relaunch()` to ensure the new app version
/// launches with a compatible daemon. This prevents the "restart twice" problem
/// where the new frontend connects to an old daemon with incompatible protocol.
/// Begin the upgrade flow by opening the dedicated upgrade window.
///
/// Saves session state for restore after relaunch and opens the upgrade screen.
#[tauri::command]
async fn begin_upgrade(
    app: tauri::AppHandle,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    log::info!("[upgrade] Beginning upgrade flow...");

    // Check if upgrade window already exists and focus it instead of creating a new one
    if let Some(existing_window) = app.get_webview_window("upgrade") {
        log::info!("[upgrade] Upgrade window already exists, focusing it");
        existing_window
            .set_focus()
            .map_err(|e| format!("Failed to focus upgrade window: {}", e))?;
        return Ok(());
    }

    // Remove stale entries before saving so ghost notebooks don't persist
    registry.prune_stale_entries(&app);

    // Save session for restore after relaunch
    session::save_session(registry.inner(), &app)?;
    log::info!("[upgrade] Session saved");

    // Create dedicated upgrade window
    tauri::WebviewWindowBuilder::new(
        &app,
        "upgrade",
        tauri::WebviewUrl::App("upgrade/index.html".into()),
    )
    .title(format!(
        "Updating {}",
        runt_workspace::desktop_display_name()
    ))
    .inner_size(500.0, 600.0)
    .min_inner_size(500.0, 400.0)
    .resizable(true)
    .center()
    .build()
    .map_err(|e| format!("Failed to create upgrade window: {}", e))?;

    log::info!("[upgrade] Upgrade window created");
    Ok(())
}

/// Get the status of all open notebooks for the upgrade screen.
///
/// Returns a list of notebooks with their kernel status, dirty state, and display name.
#[tauri::command]
async fn get_upgrade_notebook_status(
    app: tauri::AppHandle,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<Vec<UpgradeNotebookStatus>, String> {
    registry.prune_stale_entries(&app);
    // Extract data from registry without holding lock across await
    let notebook_data: Vec<(String, String, String, bool, SharedNotebookSync)> = {
        let contexts = registry.contexts.lock().map_err(|e| e.to_string())?;
        contexts
            .iter()
            .filter(|(label, _)| *label != "onboarding" && *label != "upgrade")
            .filter_map(|(label, context)| {
                let path = context.path.lock().ok()?;
                let notebook_id = context.notebook_id.lock().ok()?.clone();
                let is_dirty = context.dirty.load(Ordering::SeqCst);
                let display_name = path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "Untitled".to_string());
                Some((
                    label.clone(),
                    notebook_id,
                    display_name,
                    is_dirty,
                    context.notebook_sync.clone(),
                ))
            })
            .collect()
    };

    // Now do async operations without holding the std::sync::Mutex
    let mut statuses = Vec::new();
    for (window_label, notebook_id, display_name, is_dirty, notebook_sync) in notebook_data {
        let kernel_status = {
            let guard = notebook_sync.lock().await;
            if let Some(handle) = guard.as_ref() {
                match handle.send_request(NotebookRequest::GetKernelInfo {}).await {
                    Ok(NotebookResponse::KernelInfo { status, .. }) => Some(status),
                    _ => None,
                }
            } else {
                None
            }
        };

        statuses.push(UpgradeNotebookStatus {
            window_label,
            notebook_id,
            display_name,
            kernel_status,
            is_dirty,
        });
    }

    log::info!(
        "[upgrade] Found {} notebooks for upgrade status",
        statuses.len()
    );
    Ok(statuses)
}

/// Shutdown a kernel for upgrade.
///
/// Forcefully shuts down the kernel (sends SIGKILL to process group).
/// This is more reliable than interrupt for stopping blocking operations.
#[tauri::command]
async fn abort_kernel_for_upgrade(
    window_label: String,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    log::info!(
        "[upgrade] Shutting down kernel for window: {}",
        window_label
    );

    let context = registry.get(&window_label)?;
    let guard = context.notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;

    handle
        .send_request(NotebookRequest::ShutdownKernel {})
        .await
        .map_err(|e| format!("shutdown failed: {}", e))?;

    log::info!("[upgrade] Kernel shutdown for window: {}", window_label);
    Ok(())
}

/// Execute the full upgrade sequence.
///
/// Steps:
/// 1. Save all dirty notebooks
/// 2. Shutdown all kernels
/// 3. Close all notebook windows
/// 4. Upgrade the daemon
/// 5. Signal ready for restart
#[tauri::command]
async fn run_upgrade(
    app: tauri::AppHandle,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    log::info!("[upgrade] Starting upgrade sequence...");

    // Save session now in case begin_upgrade() missed windows that were still
    // loading, or new windows were opened between begin_upgrade() and this call.
    // This overwrites the file begin_upgrade() wrote, which is fine — the
    // registry is strictly a superset at this point.
    registry.prune_stale_entries(&app);
    if let Err(e) = session::save_session(registry.inner(), &app) {
        log::warn!("[upgrade] Failed to re-save session: {}", e);
        // Non-fatal — begin_upgrade() already saved a session
    }

    // Step 1: Save all dirty notebooks
    app.emit("upgrade:progress", UpgradeProgress::SavingNotebooks)
        .map_err(|e| e.to_string())?;

    // Extract notebooks to save (those that are dirty and have a path)
    let notebooks_to_save: Vec<(String, SharedNotebookSync, Arc<AtomicBool>)> = {
        let contexts = registry.contexts.lock().map_err(|e| e.to_string())?;
        contexts
            .iter()
            .filter(|(label, _)| *label != "onboarding" && *label != "upgrade")
            .filter_map(|(label, context)| {
                let is_dirty = context.dirty.load(Ordering::SeqCst);
                let has_path = context.path.lock().map(|p| p.is_some()).unwrap_or(false);
                if is_dirty && has_path {
                    Some((
                        label.clone(),
                        context.notebook_sync.clone(),
                        context.dirty.clone(),
                    ))
                } else {
                    None
                }
            })
            .collect()
    };

    // Save each notebook
    for (label, notebook_sync, dirty) in notebooks_to_save {
        let guard = notebook_sync.lock().await;
        if let Some(handle) = guard.as_ref() {
            match handle
                .send_request(NotebookRequest::SaveNotebook {
                    format_cells: false,
                    path: None,
                })
                .await
            {
                Ok(NotebookResponse::NotebookSaved { path, .. }) => {
                    log::info!("[upgrade] Saved notebook: {}", path);
                    dirty.store(false, Ordering::SeqCst);
                }
                Ok(NotebookResponse::Error { error }) => {
                    log::warn!("[upgrade] Failed to save notebook {}: {}", label, error);
                }
                _ => {}
            }
        }
    }

    // Step 2: Shutdown all runtimes
    app.emit("upgrade:progress", UpgradeProgress::StoppingRuntimes)
        .map_err(|e| e.to_string())?;

    // Extract sync handles for kernel shutdown
    let kernel_handles: Vec<(String, SharedNotebookSync)> = {
        let contexts = registry.contexts.lock().map_err(|e| e.to_string())?;
        contexts
            .iter()
            .filter(|(label, _)| *label != "onboarding" && *label != "upgrade")
            .map(|(label, context)| (label.clone(), context.notebook_sync.clone()))
            .collect()
    };

    // Shutdown each kernel
    for (label, notebook_sync) in kernel_handles {
        let guard = notebook_sync.lock().await;
        if let Some(handle) = guard.as_ref() {
            match handle
                .send_request(NotebookRequest::ShutdownKernel {})
                .await
            {
                Ok(_) => log::info!("[upgrade] Shutdown kernel for: {}", label),
                Err(e) => log::warn!("[upgrade] Failed to shutdown kernel {}: {}", label, e),
            }
        }
    }

    // Step 3: Close all notebook windows (keep upgrade window)
    app.emit("upgrade:progress", UpgradeProgress::ClosingWindows)
        .map_err(|e| e.to_string())?;

    // Collect window labels to close
    let windows_to_close: Vec<String> = app
        .webview_windows()
        .keys()
        .filter(|label| *label != "upgrade")
        .cloned()
        .collect();

    // Clear sync handles using the existing pattern
    let handles_to_clear: Vec<(String, SharedNotebookSync)> = {
        let mut contexts = registry.contexts.lock().map_err(|e| e.to_string())?;
        windows_to_close
            .iter()
            .filter_map(|label| {
                contexts
                    .remove(label)
                    .map(|ctx| (label.clone(), ctx.notebook_sync))
            })
            .collect()
    };

    // Clear each sync handle
    for (label, notebook_sync) in handles_to_clear {
        let mut guard = notebook_sync.lock().await;
        if guard.take().is_some() {
            log::info!("[upgrade] Cleared sync handle for: {}", label);
        }
    }

    // Close the windows
    for label in &windows_to_close {
        if let Some(window) = app.get_webview_window(label) {
            let _ = window.close();
            log::info!("[upgrade] Closed window: {}", label);
        }
    }

    // Step 4: Upgrade daemon.
    // At this point, Tauri's downloadAndInstall() has already completed
    // (called by the upgrade frontend before invoking run_upgrade).
    // The sidecar should resolve to the NEW binary if the app bundle
    // was replaced on disk. If not, the startup version check will
    // catch the mismatch on next launch.
    log::info!(
        "[upgrade] Step 4: upgrading daemon (bundled={})",
        bundled_daemon_version()
    );
    app.emit("upgrade:progress", UpgradeProgress::UpgradingDaemon)
        .map_err(|e| e.to_string())?;

    if let Err(e) = upgrade_daemon_via_sidecar(&app, |progress| {
        log::info!("[upgrade] Daemon progress: {:?}", progress);
    })
    .await
    {
        log::error!("[upgrade] Daemon upgrade failed: {}", e);
        app.emit(
            "upgrade:progress",
            UpgradeProgress::Failed { error: e.clone() },
        )
        .map_err(|e| e.to_string())?;
        return Err(e);
    }

    // Step 5: Re-install CLI if it was previously installed (ensures Windows
    // copies and Unix symlinks point at the new app bundle).
    // Local installs: symlinks updated, legacy installs migrated to ~/.local/bin.
    if cli_install::is_cli_installed_local() || cli_install::is_cli_installed_legacy() {
        match cli_install::install_cli(&app) {
            Ok(()) => log::info!("[upgrade] Local CLI re-installed successfully"),
            Err(e) => log::warn!("[upgrade] Local CLI re-install failed (non-fatal): {}", e),
        }
    }
    // System-wide installs are copies (not symlinks), so they need updating too.
    if cli_install::is_cli_installed_system() {
        match cli_install::install_cli_system(&app) {
            Ok(()) => log::info!("[upgrade] System-wide CLI re-installed successfully"),
            Err(e) => log::warn!(
                "[upgrade] System-wide CLI re-install failed (non-fatal): {}",
                e
            ),
        }
    }

    // Step 6: Signal ready
    app.emit("upgrade:progress", UpgradeProgress::Ready)
        .map_err(|e| e.to_string())?;

    log::info!("[upgrade] Upgrade complete, ready for restart");
    Ok(())
}

/// Ensure the daemon is running using Tauri's sidecar API.
///
/// This replaces the old `ensure_daemon_running` flow that used ServiceManager directly.
/// The new flow:
/// 1. Ping to check if daemon is running
/// 2. If not, spawn `runtimed install` via sidecar (which also starts it)
/// 3. Wait for daemon to become ready
/// 4. Emit progress events throughout
async fn ensure_daemon_via_sidecar<F>(
    app: &tauri::AppHandle,
    on_progress: F,
) -> Result<String, String>
where
    F: Fn(runtimed::client::DaemonProgress) + Clone + Send + 'static,
{
    use runtimed::client::{DaemonProgress, PoolClient};
    use tauri_plugin_shell::{process::CommandEvent, ShellExt};

    let bundled_version = bundled_daemon_version();
    log::info!(
        "[startup] Checking if daemon is running... (bundled={})",
        bundled_version
    );
    on_progress(DaemonProgress::Checking);

    // Check if daemon is already running
    let client = PoolClient::default();
    if let Ok(()) = client.ping().await {
        // Daemon is running - check version alignment (production only)
        if !runt_workspace::is_dev_mode() {
            if let Some(info) = runtimed::singleton::get_running_daemon_info() {
                // Compare commit hashes only - CI appends "+{git_sha}" to the version
                // at build time, so commit hash is the precise compatibility check.
                let running_commit = extract_commit_hash(&info.version);
                let bundled_commit = extract_commit_hash(&bundled_version);

                if running_commit != bundled_commit {
                    log::info!(
                        "[startup] Daemon commit mismatch — will upgrade: running={}, bundled={}",
                        info.version,
                        bundled_version
                    );
                    // Upgrade daemon to match bundled version
                    return upgrade_daemon_via_sidecar(app, on_progress).await;
                }
                log::info!(
                    "[startup] Daemon version aligned: running={}, bundled={}",
                    info.version,
                    bundled_version
                );
            } else {
                log::warn!(
                    "[startup] Daemon responded to ping but info file missing (bundled={})",
                    bundled_version
                );
            }
        }

        let endpoint = runt_workspace::default_socket_path()
            .to_string_lossy()
            .to_string();
        log::info!("[startup] Daemon already running at {}", endpoint);
        on_progress(DaemonProgress::Ready {
            endpoint: endpoint.clone(),
        });
        return Ok(endpoint);
    }

    // In dev mode, don't auto-install - user should run dev-daemon manually
    if runt_workspace::is_dev_mode() {
        log::info!("[startup] Dev mode: daemon not running, skipping auto-install");
        let guidance = "Start it with: cargo xtask dev-daemon".to_string();
        on_progress(DaemonProgress::Failed {
            error: "Dev daemon not running".to_string(),
            guidance: guidance.clone(),
        });
        return Err(format!(
            "Dev daemon not running at {:?}. {}",
            runt_workspace::default_socket_path(),
            guidance
        ));
    }

    // Daemon not running - spawn sidecar to install and start
    log::info!("[startup] Daemon not responding, spawning runtimed install via sidecar...");
    on_progress(DaemonProgress::Installing);

    // Note: Use just the binary name (not the path) for sidecar
    let sidecar_result = app
        .shell()
        .sidecar("runtimed")
        .map_err(|e| format!("Failed to create sidecar command: {}", e))?
        .args(["install"])
        .spawn();

    let (mut rx, _child) = sidecar_result.map_err(|e| format!("Failed to spawn sidecar: {}", e))?;

    // Collect output for logging
    let mut exit_code = None;
    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(line) => {
                let line_str = String::from_utf8_lossy(&line);
                log::info!("[runtimed install] {}", line_str.trim());
            }
            CommandEvent::Stderr(line) => {
                let line_str = String::from_utf8_lossy(&line);
                log::warn!("[runtimed install] {}", line_str.trim());
            }
            CommandEvent::Terminated(status) => {
                exit_code = status.code;
            }
            _ => {}
        }
    }

    // Check exit code
    if exit_code != Some(0) {
        let error = format!(
            "{} install failed with code {:?}",
            runt_workspace::daemon_binary_basename(),
            exit_code
        );
        log::error!("[startup] {}", error);
        on_progress(DaemonProgress::Failed {
            error: error.clone(),
            guidance: format!(
                "Try running: {} install",
                runt_workspace::daemon_binary_basename()
            ),
        });
        return Err(error);
    }

    log::info!("[startup] runtimed install completed, waiting for daemon to be ready...");
    on_progress(DaemonProgress::Starting);

    // Wait for daemon to become ready (up to 10 seconds)
    let max_attempts = 20;
    for attempt in 1..=max_attempts {
        on_progress(DaemonProgress::WaitingForReady {
            attempt,
            max_attempts,
        });

        if client.ping().await.is_ok() {
            let endpoint = runt_workspace::default_socket_path()
                .to_string_lossy()
                .to_string();
            log::info!(
                "[startup] Daemon ready at {} (attempt {})",
                endpoint,
                attempt
            );
            on_progress(DaemonProgress::Ready {
                endpoint: endpoint.clone(),
            });
            return Ok(endpoint);
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    // Timed out
    let error = "Daemon did not become ready within timeout".to_string();
    log::error!("[startup] {}", error);
    on_progress(DaemonProgress::Failed {
        error: error.clone(),
        guidance: format!(
            "Check daemon logs: {} daemon logs",
            runt_workspace::cli_command_name()
        ),
    });
    Err(error)
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
                runt_workspace::default_socket_path()
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
        let is_dev_mode = runt_workspace::is_dev_mode();
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

/// System info for the feedback window (not debug-gated).
#[derive(Clone, serde::Serialize)]
pub struct FeedbackSystemInfo {
    pub app_version: String,
    pub commit_sha: String,
    pub release_date: String,
    pub os: String,
    pub arch: String,
    pub os_version: String,
}

fn get_os_version() -> String {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|content| {
                content
                    .lines()
                    .find(|l| l.starts_with("PRETTY_NAME="))
                    .map(|l| {
                        l.trim_start_matches("PRETTY_NAME=")
                            .trim_matches('"')
                            .to_string()
                    })
            })
            .unwrap_or_else(|| "unknown".to_string())
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "ver"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        "unknown".to_string()
    }
}

#[tauri::command]
async fn get_feedback_system_info() -> FeedbackSystemInfo {
    FeedbackSystemInfo {
        app_version: crate::menu::APP_VERSION.to_string(),
        commit_sha: crate::menu::APP_COMMIT_SHA.to_string(),
        release_date: crate::menu::APP_RELEASE_DATE.to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        os_version: get_os_version(),
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

/// Get the OS username for peer presence labels.
#[tauri::command]
fn get_username() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default()
}

/// Complete onboarding and open a fresh notebook window.
///
/// Called from the frontend when the user finishes the onboarding flow.
/// This closes any onboarding-only window and creates a proper notebook
/// window with the correct working directory.
///
/// Settings are passed directly from the frontend to avoid race conditions
/// where the JSON settings file may not have been persisted yet by the daemon.
#[tauri::command]
async fn complete_onboarding(
    window: tauri::Window,
    app: tauri::AppHandle,
    registry: tauri::State<'_, WindowNotebookRegistry>,
    default_runtime: String,
    default_python_env: String,
) -> Result<(), String> {
    info!(
        "[onboarding] Completing onboarding with runtime={}, python_env={}",
        default_runtime, default_python_env
    );

    // Parse runtime from frontend - use Python as fallback
    let runtime: Runtime = default_runtime.parse().unwrap_or(Runtime::Python);

    // Note: default_python_env is already persisted via set_synced_setting before this call.
    // The daemon reads it from synced settings when auto-launching Python kernels.
    // We log it here for debugging but don't need to pass it further - the daemon has it.
    let _ = &default_python_env; // Explicitly mark as intentionally received but daemon-handled

    // Use notebooks directory as working directory for the new notebook
    let working_dir = ensure_notebooks_directory().ok();

    // Create the notebook window using daemon-owned creation
    let label = create_notebook_window_for_daemon(
        &app,
        registry.inner(),
        OpenMode::Create {
            runtime: runtime.to_string(),
            working_dir,
            notebook_id: None,
        },
        None,
    )?;
    info!("[onboarding] Created notebook window with label: {}", label);

    // Close the onboarding window (the one that called this command)
    window.close().map_err(|e| e.to_string())?;

    Ok(())
}

/// Check if the notebook has a file path set
#[tauri::command]
async fn has_notebook_path(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<bool, String> {
    let path = path_for_window(&window, registry.inner())?;
    let path = path.lock().map_err(|e| e.to_string())?;
    Ok(path.is_some())
}

/// Clear the Tauri-side dirty flag. Called by the frontend when the daemon
/// autosaves the notebook, so the flag stays in sync without a full save round-trip.
#[tauri::command]
fn mark_notebook_clean(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let dirty = dirty_for_window(&window, registry.inner())?;
    dirty.store(false, Ordering::SeqCst);
    Ok(())
}

/// Format all code cells in the notebook and save.
/// Formatting is best-effort - cells that fail to format are saved as-is.
///
/// The daemon handles both formatting and disk persistence:
/// - Formats code cells using ruff (Python) or deno fmt (Deno)
/// - Updates the Automerge doc with formatted sources (synced to all clients)
/// - Writes the .ipynb file to disk
#[tauri::command]
async fn save_notebook(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let path = path_for_window(&window, registry.inner())?;
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let dirty = dirty_for_window(&window, registry.inner())?;

    // Verify we have a path - daemon will use the room's notebook_path
    {
        let p = path.lock().map_err(|e| e.to_string())?;
        if p.is_none() {
            return Err("No file path set - use save_notebook_as".to_string());
        }
    }

    // Save via daemon - daemon handles formatting and disk write
    // Formatted sources are synced back via Automerge
    let sync_handle = notebook_sync.lock().await.clone();
    let handle = sync_handle.ok_or("Not connected to daemon")?;

    match handle
        .send_request(NotebookRequest::SaveNotebook {
            format_cells: true, // Daemon formats cells before saving
            path: None,         // Use room's notebook_path
        })
        .await
    {
        Ok(NotebookResponse::NotebookSaved { path, .. }) => {
            info!("[save] Notebook saved via daemon to: {}", path);
        }
        Ok(NotebookResponse::Error { error }) => {
            return Err(format!("Daemon save failed: {}", error));
        }
        Ok(other) => {
            return Err(format!("Unexpected daemon response: {:?}", other));
        }
        Err(e) => {
            return Err(format!("Daemon request failed: {}", e));
        }
    }

    // Mark as clean
    dirty.store(false, Ordering::SeqCst);
    Ok(())
}

/// Save notebook to a specific path (Save As).
///
/// The daemon handles both formatting and disk persistence:
/// - Formats code cells using ruff (Python) or deno fmt (Deno)
/// - Updates the Automerge doc with formatted sources (synced to all clients)
/// - Writes the .ipynb file to the specified path
///
/// Uses the daemon-returned path (which may have .ipynb appended) as the
/// canonical path for window title, state, and room reconnection.
#[tauri::command]
async fn save_notebook_as(
    path: String,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let context_path = path_for_window(&window, registry.inner())?;
    let dirty = dirty_for_window(&window, registry.inner())?;

    let sync_handle = notebook_sync.lock().await.clone();
    let handle = sync_handle.ok_or("Not connected to daemon")?;

    // Save via daemon — daemon writes to disk and re-keys the room from
    // UUID → canonical file path. The connection stays live, no reconnect needed.
    let (saved_path, new_notebook_id) = match handle
        .send_request(NotebookRequest::SaveNotebook {
            format_cells: true,
            path: Some(path),
        })
        .await
    {
        Ok(NotebookResponse::NotebookSaved {
            path: daemon_path,
            new_notebook_id,
        }) => {
            info!("[save-as] Notebook saved via daemon to: {}", daemon_path);
            (PathBuf::from(daemon_path), new_notebook_id)
        }
        Ok(NotebookResponse::Error { error }) => {
            return Err(format!("Daemon save failed: {}", error));
        }
        Ok(other) => {
            return Err(format!("Unexpected daemon response: {:?}", other));
        }
        Err(e) => {
            return Err(format!("Daemon request failed: {}", e));
        }
    };

    // Update local state — the room is the same, just aliased to a file path now.
    let filename = saved_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Untitled.ipynb");
    let _ = window.set_title(filename);

    if let Ok(mut p) = context_path.lock() {
        *p = Some(saved_path.clone());
    }
    dirty.store(false, Ordering::SeqCst);

    if let Some(ref new_id) = new_notebook_id {
        let notebook_id_arc = notebook_id_for_window(&window, registry.inner())?;
        if let Ok(mut id) = notebook_id_arc.lock() {
            info!("[save-as] Room re-keyed: {} -> {}", *id, new_id);
            *id = new_id.clone();
        }
        drop(notebook_id_arc);

        // Update the live relay handle's notebook_id so subsequent commands
        // (e.g. user-triggered kernel restart) derive the correct file path
        // instead of the stale UUID.
        let guard = notebook_sync.lock().await;
        if let Some(ref handle) = *guard {
            handle.set_notebook_id(new_id.clone());
        }
        drop(guard);
    }

    refresh_native_menu(window.app_handle(), registry.inner());

    // Restart the kernel only if one was already running. This preserves
    // trust: if the user had a kernel, trust was already approved. If not,
    // we don't bypass the trust dialog by launching one now.
    let saved_path_str = saved_path.to_string_lossy().to_string();
    let notebook_sync_for_kernel = notebook_sync.clone();
    tokio::spawn(async move {
        let guard = notebook_sync_for_kernel.lock().await;
        if let Some(ref handle) = *guard {
            match handle
                .send_request(NotebookRequest::ShutdownKernel {})
                .await
            {
                Ok(NotebookResponse::KernelShuttingDown {}) => {
                    // Had a running kernel — relaunch with the correct path.
                    match handle
                        .send_request(NotebookRequest::LaunchKernel {
                            kernel_type: "auto".to_string(),
                            env_source: "auto".to_string(),
                            notebook_path: Some(saved_path_str),
                        })
                        .await
                    {
                        Ok(resp) => {
                            info!("[save-as] Kernel launched for saved notebook: {:?}", resp)
                        }
                        Err(e) => warn!("[save-as] Kernel launch failed: {}", e),
                    }
                }
                _ => {
                    // No kernel was running — don't launch one (trust not yet approved).
                    info!("[save-as] No kernel was running, skipping launch");
                }
            }
        }
    });

    Ok(())
}

/// Clone the current notebook for saving as a new file.
/// The daemon handles generating fresh env_id and clearing outputs/execution counts.
#[tauri::command]
async fn clone_notebook_to_path(
    path: String,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;

    // Clone via daemon - daemon reads from Automerge, clears outputs, generates fresh env_id
    let sync_handle = notebook_sync.lock().await.clone();
    let handle = sync_handle.ok_or("Not connected to daemon")?;

    match handle
        .send_request(NotebookRequest::CloneNotebook { path })
        .await
    {
        Ok(NotebookResponse::NotebookCloned { path: cloned_path }) => {
            info!("[clone] Notebook cloned via daemon to: {}", cloned_path);
            Ok(())
        }
        Ok(NotebookResponse::Error { error }) => Err(format!("Daemon clone failed: {}", error)),
        Ok(other) => Err(format!("Unexpected daemon response: {:?}", other)),
        Err(e) => Err(format!("Daemon request failed: {}", e)),
    }
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

/// Create a notebook window using daemon-owned loading.
///
/// The window is created immediately (with a loading state). The daemon connection
/// happens asynchronously — `notebook_id` is updated when the daemon responds.
fn create_notebook_window_for_daemon(
    app: &tauri::AppHandle,
    registry: &WindowNotebookRegistry,
    mode: OpenMode,
    custom_label: Option<String>,
) -> Result<String, String> {
    // Extract window metadata from the mode
    let (title, path, working_dir, runtime) = match &mode {
        OpenMode::Open { path } => {
            let title = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Untitled.ipynb")
                .to_string();
            let runtime = settings::load_settings().default_runtime;
            (title, Some(path.clone()), None, runtime)
        }
        OpenMode::Create {
            runtime,
            working_dir,
            ..
        } => {
            let runtime_enum: Runtime = runtime.parse().unwrap_or(Runtime::Python);
            (
                "Untitled.ipynb".to_string(),
                None,
                working_dir.clone(),
                runtime_enum,
            )
        }
    };

    // Generate a stable window label for the window-state plugin
    let label = custom_label.unwrap_or_else(|| {
        if let OpenMode::Create {
            notebook_id: Some(ref id),
            ..
        } = &mode
        {
            format!("notebook-{}", &id[..8.min(id.len())])
        } else if let Some(ref p) = path {
            let hash = runt_workspace::worktree_hash(p);
            format!("notebook-{}", &hash[..8])
        } else {
            format!("notebook-{}", uuid::Uuid::new_v4())
        }
    });

    // Remove registry entries for windows that no longer exist. Without this,
    // ghost notebooks appear in the upgrade dialog and saved session.
    log::debug!(
        "[window] Pruning stale entries before creating window '{}'",
        label
    );
    registry.prune_stale_entries(app);

    // If a window with this label already exists, focus it instead of opening a
    // duplicate. Opening the same file in multiple windows causes state
    // inconsistencies (dirty flags, titles, session restore). See #1173.
    if let Some(existing) = app.get_webview_window(&label) {
        info!(
            "[window] Focusing existing window '{}' instead of opening duplicate",
            label
        );
        let _ = existing.show();
        let _ = existing.unminimize();
        let _ = existing.set_focus();
        return Ok(label);
    }

    // Placeholder notebook_id — daemon will provide the canonical one.
    let placeholder_id = match &mode {
        OpenMode::Open { path } => path
            .canonicalize()
            .unwrap_or_else(|_| path.clone())
            .to_string_lossy()
            .to_string(),
        OpenMode::Create {
            notebook_id: Some(ref id),
            ..
        } => id.clone(),
        OpenMode::Create {
            notebook_id: None, ..
        } => String::new(),
    };

    let context =
        create_window_context_for_daemon(path, working_dir.clone(), placeholder_id, runtime);
    // If insert fails due to a label collision (race between window check and insert),
    // retry with a unique suffix (#577).
    let label = if registry.insert(label.clone(), context.clone()).is_err() {
        let suffixed = format!("{}-{}", label, &uuid::Uuid::new_v4().to_string()[..8]);
        registry.insert(suffixed.clone(), context.clone())?;
        suffixed
    } else {
        label
    };

    let username = get_username();
    let init_script = format!(
        "window.__NTERACT_USERNAME__ = {};",
        serde_json::to_string(&username).unwrap_or_else(|_| "\"\"".to_string())
    );

    let window =
        match tauri::WebviewWindowBuilder::new(app, label.clone(), tauri::WebviewUrl::default())
            .title(&title)
            .initialization_script(&init_script)
            .inner_size(1100.0, 750.0)
            .min_inner_size(400.0, 250.0)
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

    // Spawn async daemon connection — window shows loading state until daemon:ready
    let notebook_sync = context.notebook_sync;
    let sync_generation = context.sync_generation;
    let notebook_id_arc = context.notebook_id;
    tauri::async_runtime::spawn(async move {
        let result = match mode {
            OpenMode::Open { path } => {
                initialize_notebook_sync_open(
                    window,
                    path,
                    notebook_sync,
                    sync_generation,
                    notebook_id_arc,
                )
                .await
            }
            OpenMode::Create {
                runtime,
                working_dir,
                notebook_id,
            } => {
                initialize_notebook_sync_create(
                    window,
                    runtime,
                    working_dir,
                    notebook_id,
                    notebook_sync,
                    sync_generation,
                    notebook_id_arc,
                )
                .await
            }
        };
        if let Err(e) = result {
            warn!("[startup] Daemon notebook sync failed: {}", e);
        }
    });

    refresh_native_menu(app, registry);
    Ok(label)
}

fn open_notebook_window(
    app: &tauri::AppHandle,
    registry: &WindowNotebookRegistry,
    path: &Path,
) -> Result<(), String> {
    create_notebook_window_for_daemon(
        app,
        registry,
        OpenMode::Open {
            path: path.to_path_buf(),
        },
        None,
    )
    .map(|_| ())
}

/// Process a single file-open URL: focus existing window, reuse empty window, or open new.
/// Extracted from RunEvent::Opened handler so it can be reused for deferred URLs.
#[cfg(target_os = "macos")]
fn handle_open_url(
    app_handle: &tauri::AppHandle,
    registry: &WindowNotebookRegistry,
    url: &tauri::Url,
) {
    let path = match url.scheme() {
        "file" => url.to_file_path().ok(),
        _ => None,
    };
    let Some(path) = path else { return };
    if path.extension().and_then(|e| e.to_str()) != Some("ipynb") {
        return;
    }

    // Focus an existing window for this notebook if one is open.
    if let Some(label) = registry.find_label_by_path(&path) {
        if let Some(existing) = app_handle.get_webview_window(&label) {
            log::info!(
                "[file-open] Focusing existing window '{}' for {}",
                label,
                path.display()
            );
            let _ = existing.set_focus();
            return;
        }
    }

    // Reuse an empty (untitled) window if one exists, otherwise open new.
    if let Some(empty_label) = registry.find_empty_window_label(app_handle) {
        if let Ok(context) = registry.get(&empty_label) {
            // Update path in context
            if let Ok(mut p) = context.path.lock() {
                *p = Some(path.clone());
            }

            if let Some(window) = app_handle.get_webview_window(&empty_label) {
                log::info!(
                    "[file-open] Reusing empty window '{}' for {}",
                    empty_label,
                    path.display()
                );
                let title = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("Untitled.ipynb");
                let _ = window.set_title(title);
                refresh_native_menu(app_handle, registry);

                // Disconnect existing sync and reconnect with the file path
                let notebook_sync = context.notebook_sync.clone();
                let sync_generation = context.sync_generation.clone();
                let notebook_id = context.notebook_id.clone();
                let open_path = path.clone();
                tauri::async_runtime::spawn(async move {
                    // Clear existing handle
                    *notebook_sync.lock().await = None;
                    if let Err(e) = initialize_notebook_sync_open(
                        window,
                        open_path,
                        notebook_sync,
                        sync_generation,
                        notebook_id,
                    )
                    .await
                    {
                        log::error!("[file-open] Daemon sync failed for reused window: {}", e);
                    }
                });
            }
        }
    } else if let Err(e) = open_notebook_window(app_handle, registry, &path) {
        log::error!("[file-open] Failed to open notebook in new window: {}", e);
    }
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

/// Sync environment via the daemon - hot-install new packages without restart.
/// Only supported for UV inline deps.
#[tauri::command]
async fn sync_environment_via_daemon(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<NotebookResponse, String> {
    info!("[daemon-kernel] sync_environment_via_daemon");

    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;

    handle
        .send_request(NotebookRequest::SyncEnvironment {})
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

/// Get the last daemon progress status (for UI to check on mount).
#[tauri::command]
fn get_daemon_status(
    status_state: tauri::State<'_, DaemonStatusState>,
) -> Option<runtimed::client::DaemonProgress> {
    status_state.0.lock().ok().and_then(|guard| guard.clone())
}

/// Get pool statistics from the daemon.
/// Returns pool state from the daemon's PoolDoc.
#[tauri::command]
async fn get_pool_status() -> Result<runtimed::PoolState, String> {
    let client = runtimed::client::PoolClient::default();
    client
        .status()
        .await
        .map_err(|e| format!("Failed to get pool status: {}", e))
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
    debug!(
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
    debug!(
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

/// Check if an error indicates the daemon is dead (socket missing or connection refused).
fn is_daemon_dead_error(error: &str) -> bool {
    error.contains("No such file or directory")
        || error.contains("Connection refused")
        || error.contains("os error 2")
        || error.contains("os error 111")
}

/// Reconnect to the daemon after a disconnection.
///
/// If the socket doesn't exist (daemon dead), this will attempt to restart the daemon
/// using `ensure_daemon_via_sidecar()` before retrying the connection.
/// In dev mode, returns a helpful error instead of attempting recovery.
///
/// Called by the frontend after receiving daemon:disconnected event.
#[tauri::command]
async fn reconnect_to_daemon(
    window: tauri::Window,
    app: tauri::AppHandle,
    registry: tauri::State<'_, WindowNotebookRegistry>,
    reconnect_in_progress: tauri::State<'_, ReconnectInProgress>,
    restart_in_progress: tauri::State<'_, DaemonRestartInProgress>,
) -> Result<(), String> {
    info!("[daemon-kernel] reconnect_to_daemon");

    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let sync_generation = sync_generation_for_window(&window, registry.inner())?;
    let working_dir = working_dir_for_window(&window, registry.inner())?;
    let context_path = path_for_window(&window, registry.inner())?;
    let context_notebook_id = notebook_id_for_window(&window, registry.inner())?;

    let path = context_path.lock().map_err(|e| e.to_string())?.clone();
    let notebook_id = context_notebook_id
        .lock()
        .map_err(|e| e.to_string())?
        .clone();

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

    let webview_window = window
        .app_handle()
        .get_webview_window(window.label())
        .ok_or_else(|| "Current webview window not found".to_string())?;

    let context = registry.get(window.label())?;
    let runtime = context.runtime.to_string();

    // First attempt: try to connect (daemon might have restarted)
    let result = if let Some(ref p) = path {
        info!(
            "[daemon-kernel] Reconnecting via OpenNotebook: {}",
            p.display()
        );
        initialize_notebook_sync_open(
            webview_window.clone(),
            p.clone(),
            notebook_sync.clone(),
            sync_generation.clone(),
            context_notebook_id.clone(),
        )
        .await
    } else {
        info!(
            "[daemon-kernel] Reconnecting untitled notebook: {}",
            notebook_id
        );
        initialize_notebook_sync_create(
            webview_window.clone(),
            runtime.clone(),
            working_dir.clone(),
            Some(notebook_id.clone()),
            notebook_sync.clone(),
            sync_generation.clone(),
            context_notebook_id.clone(),
        )
        .await
    };

    match result {
        Ok(()) => {
            reset_flag();
            Ok(())
        }
        Err(ref e) if is_daemon_dead_error(e) => {
            info!("[daemon-kernel] Daemon appears dead: {}", e);

            // In dev mode, don't attempt recovery - show helpful guidance
            if runt_workspace::is_dev_mode() {
                reset_flag();
                return Err(
                    "Dev daemon not running. Start it with: cargo xtask dev-daemon".to_string(),
                );
            }

            // Try to acquire the restart flag (only one window should restart)
            if restart_in_progress
                .0
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                info!("[daemon-kernel] Attempting to restart daemon...");

                // Attempt daemon restart
                let restart_result = ensure_daemon_via_sidecar(&app, |progress| {
                    info!("[daemon-kernel] Daemon restart progress: {:?}", progress);
                })
                .await;

                restart_in_progress.0.store(false, Ordering::SeqCst);

                if let Err(restart_err) = restart_result {
                    reset_flag();
                    return Err(format!("Failed to restart daemon: {}", restart_err));
                }

                info!("[daemon-kernel] Daemon restarted, retrying connection...");
            } else {
                // Another window is restarting, wait for it (up to 30s)
                info!("[daemon-kernel] Another window is restarting daemon, waiting...");
                for _ in 0..60 {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    if !restart_in_progress.0.load(Ordering::SeqCst) {
                        break;
                    }
                }
            }

            // Wait for daemon to be ready before retrying connection
            let client = runtimed::client::PoolClient::default();
            for attempt in 1..=20 {
                if client.ping().await.is_ok() {
                    info!(
                        "[daemon-kernel] Daemon ready after {} ping attempts",
                        attempt
                    );
                    break;
                }
                if attempt < 20 {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }

            // Retry connection after restart
            let retry_result = if let Some(p) = path {
                initialize_notebook_sync_open(
                    webview_window,
                    p,
                    notebook_sync,
                    sync_generation,
                    context_notebook_id,
                )
                .await
            } else {
                initialize_notebook_sync_create(
                    webview_window,
                    runtime,
                    working_dir,
                    Some(notebook_id),
                    notebook_sync,
                    sync_generation,
                    context_notebook_id,
                )
                .await
            };

            reset_flag();
            retry_result
        }
        Err(e) => {
            // Non-daemon-dead error, return as-is
            reset_flag();
            Err(e)
        }
    }
}

/// Signal that the frontend SyncEngine is ready to receive frames.
///
/// The Tauri frame relay buffers daemon frames until this is called,
/// preventing frame loss when the relay starts emitting before the
/// JS `SyncEngine` has subscribed to `notebook:frame` events.
///
/// Called once after `engine.start()` in `useAutomergeNotebook`. On
/// reconnection the flag persists, so the new relay proceeds immediately.
#[tauri::command]
fn notify_sync_ready(window: tauri::Window, sync_ready: tauri::State<'_, SyncReadyState>) {
    info!(
        "[notebook-sync] Frontend sync ready for '{}'",
        window.label()
    );
    sync_ready.set_ready(window.label());
}

/// Send a typed frame to the daemon.
///
/// The first byte is the frame type, the rest is the payload.
/// Supported outgoing types:
/// - 0x00: AutomergeSync (forwarded via RelayHandle::forward_frame)
/// - 0x04: Presence (forwarded via RelayHandle::forward_frame)
///
/// Accepts raw binary via `tauri::ipc::Request` — the frontend passes a
/// `Uint8Array` directly as the invoke payload, bypassing JSON serialization.
#[tauri::command]
async fn send_frame(
    request: tauri::ipc::Request<'_>,
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    let frame_data = match request.body() {
        tauri::ipc::InvokeBody::Raw(bytes) => bytes.as_slice(),
        tauri::ipc::InvokeBody::Json(value) => {
            // Backward compatibility: accept JSON array of bytes.
            // This path is slower and should be removed once all callers
            // migrate to raw binary.
            warn!("[send_frame] Received JSON payload — callers should send Uint8Array directly");
            return match serde_json::from_value::<Vec<u8>>(
                value.get("frameData").cloned().unwrap_or(value.clone()),
            ) {
                Ok(bytes) => send_frame_bytes(&bytes, &window, &registry).await,
                Err(e) => Err(format!("Failed to parse JSON frame data: {}", e)),
            };
        }
    };

    send_frame_bytes(frame_data, &window, &registry).await
}

async fn send_frame_bytes(
    frame_data: &[u8],
    window: &tauri::Window,
    registry: &tauri::State<'_, WindowNotebookRegistry>,
) -> Result<(), String> {
    if frame_data.is_empty() {
        return Err("Empty frame".to_string());
    }

    let notebook_sync = notebook_sync_for_window(window, registry.inner())?;
    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;

    use notebook_doc::frame_types;

    let frame_type = frame_data[0];
    let payload = &frame_data[1..];

    match frame_type {
        frame_types::AUTOMERGE_SYNC
        | frame_types::PRESENCE
        | frame_types::RUNTIME_STATE_SYNC
        | frame_types::POOL_STATE_SYNC => handle
            .forward_frame(frame_type, payload.to_vec())
            .await
            .map_err(|e| format!("send_frame(0x{:02x}): {}", frame_type, e)),
        _ => Err(format!(
            "Unsupported outgoing frame type: 0x{:02x}",
            frame_type
        )),
    }
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
    let path = path_for_window(&window, registry.inner())?;
    let notebook_path = {
        let path = path.lock().map_err(|e| e.to_string())?;
        path.clone()
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
    let path = path_for_window(&window, registry.inner())?;
    let notebook_path = {
        let path = path.lock().map_err(|e| e.to_string())?;
        path.clone()
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
    let path = path_for_window(&window, registry.inner())?;
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let notebook_path = {
        let path = path.lock().map_err(|e| e.to_string())?;
        path.clone()
    };

    let Some(notebook_path) = notebook_path else {
        return Err("No notebook path set".to_string());
    };

    let Some(pyproject_path) = pyproject::find_pyproject(&notebook_path) else {
        return Err("No pyproject.toml found".to_string());
    };

    let config = pyproject::parse_pyproject(&pyproject_path).map_err(|e| e.to_string())?;

    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;
    let snapshot = get_metadata_snapshot(handle).await;
    let mut metadata = snapshot
        .as_ref()
        .map(metadata_from_snapshot)
        .unwrap_or_else(|| metadata_from_snapshot(&default_metadata_snapshot()));
    let all_deps = pyproject::get_all_dependencies(&config);
    let deps = uv_env::NotebookDependencies {
        dependencies: all_deps.clone(),
        requires_python: config.requires_python,
        prerelease: None,
    };
    uv_env::set_dependencies(&mut metadata, &deps);
    let new_snapshot = snapshot_from_nbformat(&metadata);
    set_metadata_snapshot(handle, &new_snapshot).await?;
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
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<trust::TrustInfo, String> {
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;
    // Use raw metadata to preserve trust_signature (not in the typed RuntMetadata struct)
    let additional = get_raw_metadata_additional(handle)
        .await
        .ok_or("Failed to read metadata from daemon")?;
    trust::verify_notebook_trust(&additional)
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
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;

    // Use raw metadata to read trust-relevant fields without stripping unknown runt keys
    let additional = get_raw_metadata_additional(handle)
        .await
        .unwrap_or_default();
    let signature = trust::sign_notebook_dependencies(&additional)?;
    let timestamp = chrono::Utc::now().to_rfc3339();

    // Write trust fields directly into the raw Automerge JSON — avoids the typed
    // NotebookMetadataSnapshot round-trip which would strip trust_signature/trust_timestamp
    set_raw_trust_in_metadata(handle, &signature, &timestamp).await
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
    let path = path_for_window(&window, registry.inner())?;
    let notebook_path = {
        let path = path.lock().map_err(|e| e.to_string())?;
        path.clone()
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

// ============================================================================
// environment.yml Discovery and Environment Commands
// ============================================================================

/// Detect environment.yml near the notebook and return info about it.
#[tauri::command]
async fn detect_environment_yml(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<Option<environment_yml::EnvironmentYmlInfo>, String> {
    let path = path_for_window(&window, registry.inner())?;
    let notebook_path = {
        let path = path.lock().map_err(|e| e.to_string())?;
        path.clone()
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
    let path = path_for_window(&window, registry.inner())?;
    let notebook_path = {
        let path = path.lock().map_err(|e| e.to_string())?;
        path.clone()
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
    let path = path_for_window(&window, registry.inner())?;
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;
    let notebook_path = {
        let path = path.lock().map_err(|e| e.to_string())?;
        path.clone()
    };

    let Some(notebook_path) = notebook_path else {
        return Err("No notebook path set".to_string());
    };

    let Some(pixi_path) = pixi::find_pixi_toml(&notebook_path) else {
        return Err("No pixi.toml found".to_string());
    };

    let config = pixi::parse_pixi_toml(&pixi_path).map_err(|e| e.to_string())?;
    let conda_deps = pixi::convert_to_conda_dependencies(&config);

    let guard = notebook_sync.lock().await;
    let handle = guard.as_ref().ok_or("Not connected to daemon")?;
    let snapshot = get_metadata_snapshot(handle).await;
    let mut metadata = snapshot
        .as_ref()
        .map(metadata_from_snapshot)
        .unwrap_or_else(|| metadata_from_snapshot(&default_metadata_snapshot()));
    let deps = conda_env::CondaDependencies {
        dependencies: conda_deps.dependencies.clone(),
        channels: conda_deps.channels,
        python: conda_deps.python,
        env_id: None,
    };
    conda_env::set_dependencies(&mut metadata, &deps);
    let new_snapshot = snapshot_from_nbformat(&metadata);
    set_metadata_snapshot(handle, &new_snapshot).await?;
    info!(
        "Imported {} dependencies from pixi.toml into notebook conda metadata",
        conda_deps.dependencies.len()
    );
    Ok(())
}

// ========== Deno kernel support ==========

/// Detect deno.json/deno.jsonc near the notebook and return info about it
#[tauri::command]
async fn detect_deno_config(
    window: tauri::Window,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<Option<deno_env::DenoConfigInfo>, String> {
    let path = path_for_window(&window, registry.inner())?;
    let notebook_path = {
        let path = path.lock().map_err(|e| e.to_string())?;
        path.clone()
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

/// Update a synced setting via the daemon.
///
/// The daemon is the sole writer to settings.json to prevent race conditions
/// when multiple notebook windows are open. The daemon persists settings to disk
/// after receiving the sync message.
#[tauri::command]
async fn set_synced_setting(key: String, value: serde_json::Value) -> Result<(), String> {
    let socket_path = runt_workspace::default_socket_path();
    let mut client = runtimed::sync_client::SyncClient::connect_with_timeout(
        socket_path,
        std::time::Duration::from_millis(500),
    )
    .await
    .map_err(|e| format!("Daemon unavailable: {}. Setting not persisted.", e))?;

    client
        .put_value(&key, &value)
        .await
        .map_err(|e| format!("sync error: {}", e))?;

    Ok(())
}

/// Open the settings window.
///
/// Uses singleton pattern - focuses existing window if present, otherwise creates new one.
#[tauri::command]
async fn open_settings_window(app: tauri::AppHandle) -> Result<(), String> {
    // Singleton: focus existing window if present
    if let Some(existing_window) = app.get_webview_window("settings") {
        existing_window
            .set_focus()
            .map_err(|e| format!("Failed to focus settings window: {}", e))?;
        return Ok(());
    }

    // Create settings window
    tauri::WebviewWindowBuilder::new(
        &app,
        "settings",
        tauri::WebviewUrl::App("settings/index.html".into()),
    )
    .title(format!(
        "{} Settings",
        runt_workspace::desktop_display_name()
    ))
    .inner_size(560.0, 580.0)
    .min_inner_size(450.0, 400.0)
    .resizable(true)
    .center()
    .build()
    .map_err(|e| format!("Failed to create settings window: {}", e))?;

    Ok(())
}

/// Open the feedback window.
///
/// Uses singleton pattern - focuses existing window if present, otherwise creates new one.
#[tauri::command]
async fn open_feedback_window(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(existing_window) = app.get_webview_window("feedback") {
        existing_window
            .set_focus()
            .map_err(|e| format!("Failed to focus feedback window: {}", e))?;
        return Ok(());
    }

    tauri::WebviewWindowBuilder::new(
        &app,
        "feedback",
        tauri::WebviewUrl::App("feedback/index.html".into()),
    )
    .title("Send Feedback")
    .inner_size(480.0, 420.0)
    .min_inner_size(400.0, 385.0)
    .resizable(true)
    .center()
    .build()
    .map_err(|e| format!("Failed to create feedback window: {}", e))?;

    Ok(())
}

fn focused_window(app: &tauri::AppHandle) -> Option<tauri::WebviewWindow> {
    app.webview_windows()
        .into_values()
        .find(|window| window.is_focused().ok() == Some(true))
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReopenAction {
    RestoreWindow,
    SpawnNotebook,
}

#[cfg(any(target_os = "macos", test))]
fn reopen_action(has_visible_windows: bool, open_window_count: usize) -> Option<ReopenAction> {
    if has_visible_windows {
        return None;
    }

    if open_window_count == 0 {
        Some(ReopenAction::SpawnNotebook)
    } else {
        Some(ReopenAction::RestoreWindow)
    }
}

fn window_menu_display_name(
    app: &tauri::AppHandle,
    registry: &WindowNotebookRegistry,
    window_label: &str,
) -> String {
    if let Ok(context) = registry.get(window_label) {
        if let Ok(path) = context.path.lock() {
            return path
                .as_ref()
                .and_then(|p| p.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("Untitled.ipynb")
                .to_string();
        }
    }

    app.get_webview_window(window_label)
        .and_then(|window| window.title().ok())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| window_label.to_string())
}

fn window_menu_display_names(
    app: &tauri::AppHandle,
    registry: &WindowNotebookRegistry,
) -> HashMap<String, String> {
    app.webview_windows()
        .into_keys()
        .map(|window_label| {
            let display_name = window_menu_display_name(app, registry, &window_label);
            (window_label, display_name)
        })
        .collect()
}

fn refresh_native_menu(app: &tauri::AppHandle, registry: &WindowNotebookRegistry) {
    let window_display_names = window_menu_display_names(app, registry);
    match crate::menu::create_menu(app, &window_display_names) {
        Ok(menu) => {
            if let Err(error) = app.set_menu(menu) {
                warn!("[menu] Failed to update native menu: {}", error);
            }
        }
        Err(error) => {
            warn!("[menu] Failed to rebuild native menu: {}", error);
        }
    }
}
fn open_notebook_from_menu_without_window(
    app: &tauri::AppHandle,
    registry: &WindowNotebookRegistry,
) {
    use tauri_plugin_dialog::DialogExt;

    log::info!("[menu] File > Open triggered with no windows open");

    // On macOS, activate the app to ensure the file dialog is visible
    // when the app has no windows but is still running
    #[cfg(target_os = "macos")]
    #[allow(deprecated)]
    unsafe {
        use cocoa::appkit::NSApplication;
        use cocoa::base::nil;
        let ns_app = NSApplication::sharedApplication(nil);
        ns_app.activateIgnoringOtherApps_(true);
    }

    let app_handle = app.clone();
    let registry = registry.clone();

    app.dialog()
        .file()
        .add_filter("Jupyter Notebook", &["ipynb"])
        .pick_file(move |selected_path| {
            let Some(selected_path) = selected_path else {
                return;
            };

            let path = match selected_path.into_path() {
                Ok(path) => path,
                Err(e) => {
                    log::error!("[menu] Failed to resolve selected notebook path: {}", e);
                    return;
                }
            };

            if let Err(e) = open_notebook_window(&app_handle, &registry, &path) {
                log::error!("[menu] Failed to open notebook from File > Open: {}", e);

                let app_handle = app_handle.clone();
                let path_display = path.display().to_string();
                tauri::async_runtime::spawn(async move {
                    let _ = tauri_plugin_dialog::DialogExt::dialog(&app_handle)
                        .message(format!("Failed to open notebook '{}': {}", path_display, e))
                        .title("Open Notebook Error")
                        .kind(tauri_plugin_dialog::MessageDialogKind::Error)
                        .blocking_show();
                });
            }
        });
}

/// Create a new notebook window with the specified runtime.
fn spawn_new_notebook(
    app: &tauri::AppHandle,
    registry: &WindowNotebookRegistry,
    runtime: Runtime,
) -> Result<(), String> {
    create_notebook_window_for_daemon(
        app,
        registry,
        OpenMode::Create {
            runtime: runtime.to_string(),
            working_dir: None,
            notebook_id: None,
        },
        None,
    )
    .map(|_| ())
}

/// Ensure notebooks directory exists and return its path.
///
/// In dev mode with a workspace path, uses {workspace}/notebooks.
/// Otherwise uses ~/notebooks.
fn ensure_notebooks_directory() -> Result<PathBuf, String> {
    runt_workspace::default_notebooks_dir()
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
async fn run_settings_sync(app: tauri::AppHandle) {
    use tauri::Emitter;

    let socket_path = runt_workspace::default_socket_path();

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

// Pool state sync removed: pool state now syncs via PoolDoc (frame type 0x06)
// on each notebook connection. See handle_notebook_sync_connection in
// notebook_sync_server.rs.

/// Create initial notebook state for a new notebook, detecting project-level config for Python.
/// Create a window context for daemon-owned notebook loading.
///
/// Unlike `create_window_context`, this doesn't require a fully-parsed `NotebookState`.
/// The daemon owns the notebook content — Tauri just needs enough state for window management.
/// The `notebook_id` starts as a placeholder and is updated after the daemon responds.
fn create_window_context_for_daemon(
    path: Option<PathBuf>,
    working_dir: Option<PathBuf>,
    placeholder_notebook_id: String,
    runtime: Runtime,
) -> WindowNotebookContext {
    WindowNotebookContext {
        notebook_sync: Arc::new(tokio::sync::Mutex::new(None)),
        sync_generation: Arc::new(AtomicU64::new(0)),
        path: Arc::new(Mutex::new(path)),
        working_dir,
        dirty: Arc::new(AtomicBool::new(false)),
        notebook_id: Arc::new(Mutex::new(placeholder_notebook_id)),
        runtime,
    }
}

fn clear_notebook_sync_handles(handles: Vec<(String, SharedNotebookSync)>, reason: &'static str) {
    if handles.is_empty() {
        return;
    }

    tauri::async_runtime::spawn(async move {
        for (label, notebook_sync) in handles {
            let had_handle = {
                let mut sync_guard = notebook_sync.lock().await;
                sync_guard.take().is_some()
            };

            if had_handle {
                info!(
                    "[notebook-sync] Cleared sync handle for window {} ({})",
                    label, reason
                );
            } else {
                debug!(
                    "[notebook-sync] Sync handle already cleared for window {} ({})",
                    label, reason
                );
            }
        }
    });
}

#[cfg(target_os = "macos")]
fn clear_all_notebook_sync_handles(registry: &WindowNotebookRegistry, reason: &'static str) {
    let handles = match registry.contexts.lock() {
        Ok(contexts) => contexts
            .iter()
            .map(|(label, context)| (label.clone(), context.notebook_sync.clone()))
            .collect(),
        Err(e) => {
            warn!(
                "[notebook-sync] Failed to lock window registry for sync cleanup ({}): {}",
                reason, e
            );
            return;
        }
    };

    clear_notebook_sync_handles(handles, reason);
}

/// Migrate stale "main" window geometry to the new deterministic label.
///
/// Before commit 97b0422f, the first notebook window used a hardcoded "main" label.
/// The window-state plugin now denylists "main", orphaning its saved geometry.
/// This renames the "main" entry in `.window-state.json` so the new hash-based
/// label (`notebook-{hash}`) inherits the old geometry on first launch after upgrade.
fn migrate_main_window_state(session: &session::SessionState) {
    // Find the session entry that was previously the "main" window.
    // The old code always used "main" for the first window, so look for
    // entries with label "main" or (more commonly after the fix was applied)
    // compute what label the first entry would get.
    let main_entry = session.windows.iter().find(|w| w.label == "main");
    let Some(entry) = main_entry else {
        return;
    };

    let new_label = session::window_label_for_session(entry);

    // Compute the window-state plugin's config directory.
    // On macOS: ~/Library/Application Support/org.nteract.desktop/
    let Some(config_base) = dirs::config_dir() else {
        return;
    };
    let state_path = config_base
        .join("org.nteract.desktop")
        .join(".window-state.json");

    if !state_path.exists() {
        return;
    }

    let Ok(contents) = std::fs::read_to_string(&state_path) else {
        return;
    };
    let Ok(mut map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&contents)
    else {
        return;
    };

    // Only migrate if "main" exists and the new label doesn't already have geometry
    if let Some(main_state) = map.remove("main") {
        if !map.contains_key(&new_label) {
            log::info!(
                "[window-state] Migrating geometry from 'main' to '{}'",
                new_label
            );
            map.insert(new_label, main_state);
        } else {
            log::info!(
                "[window-state] Removed stale 'main' entry (new label already has geometry)"
            );
        }

        if let Ok(json) = serde_json::to_string_pretty(&serde_json::Value::Object(map)) {
            let _ = std::fs::write(&state_path, json);
        }
    }
}

/// Correct window size after the window-state plugin restores physical pixels
/// from a monitor with a different scale factor.
///
/// The plugin stores raw physical pixels. A window saved at 1100x750 physical
/// on a 1x external monitor would appear as 550x375 logical on a 2x Retina
/// display. This function adjusts the physical size to preserve the logical size.
fn correct_window_scale(window: &tauri::WebviewWindow, saved_scale_factor: Option<f64>) {
    let Some(saved_scale) = saved_scale_factor else {
        return;
    };
    let Ok(current_scale) = window.scale_factor() else {
        return;
    };

    let ratio = current_scale / saved_scale;
    // Only correct if the difference is significant (> 5%)
    if (ratio - 1.0).abs() < 0.05 {
        return;
    }

    let Ok(current_size) = window.inner_size() else {
        return;
    };

    // The plugin restored physical pixels from the old monitor.
    // To preserve logical size: new_physical = old_physical * (new_scale / old_scale)
    let corrected_width = (current_size.width as f64 * ratio) as u32;
    let corrected_height = (current_size.height as f64 * ratio) as u32;

    log::info!(
        "[window] Scale correction for {}: {}x{} -> {}x{} (scale {:.1} -> {:.1})",
        window.label(),
        current_size.width,
        current_size.height,
        corrected_width,
        corrected_height,
        saved_scale,
        current_scale,
    );

    let _ = window.set_size(tauri::PhysicalSize::new(corrected_width, corrected_height));
}

/// Run the notebook Tauri app.
///
/// If `notebook_path` is Some, opens that file. If None, creates a new empty notebook.
/// The `runtime` parameter specifies which runtime to use for new notebooks.
/// If None, falls back to user's default runtime from settings.
///
/// For untitled notebooks, the current working directory is captured at startup
/// for project file detection (pyproject.toml, pixi.toml, environment.yaml).
pub fn run(
    notebook_path: Option<PathBuf>,
    runtime: Option<Runtime>,
    notebook_id: Option<String>,
) -> anyhow::Result<()> {
    // Initialize logging via tauri-plugin-log — unified backend for both Rust
    // log::* macros and frontend JS log calls. Writes to notebook.log, stderr,
    // and forwards to webview console.
    let log_path = runt_workspace::default_notebook_log_path();
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    // Rotate previous session's log before the plugin opens the file.
    // tauri-plugin-log's RotationStrategy is size-based, not per-startup,
    // so we do our own rename here to preserve the previous session.
    let prev_log = log_path.with_extension("log.1");
    if log_path.exists() {
        let _ = std::fs::rename(&log_path, &prev_log);
    }

    let log_dir = log_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("log path has no parent directory"))?
        .to_path_buf();
    let log_file_name = log_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());

    let log_plugin = {
        use tauri_plugin_log::{Target, TargetKind, TimezoneStrategy};
        let mut log_builder = tauri_plugin_log::Builder::new()
            .clear_targets()
            .targets([
                // Write to our existing notebook.log location
                Target::new(TargetKind::Folder {
                    path: log_dir,
                    file_name: log_file_name,
                }),
                // Also print to stderr (matches previous dual-output behavior)
                Target::new(TargetKind::Stderr),
                // Forward Rust logs to webview console for devtools
                Target::new(TargetKind::Webview),
            ])
            .timezone_strategy(TimezoneStrategy::UseLocal)
            .level(match runt_workspace::build_channel() {
                runt_workspace::BuildChannel::Nightly => log::LevelFilter::Debug,
                runt_workspace::BuildChannel::Stable => log::LevelFilter::Info,
            })
            .format(move |out, message, record| {
                out.finish(format_args!(
                    "{} [{}] {}: {}",
                    chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                    record.level(),
                    record.target(),
                    message
                ))
            });
        // Respect RUST_LOG for level override. Supports both bare levels
        // (e.g. "debug") and module-level filters (e.g. "notebook=debug,info").
        // Module filters use the plugin's level_for() API; the global default
        // comes from the last bare level or stays at info.
        if let Ok(rust_log) = std::env::var("RUST_LOG") {
            for directive in rust_log.split(',') {
                let directive = directive.trim();
                if directive.is_empty() {
                    continue;
                }
                if let Some((module, level_str)) = directive.split_once('=') {
                    if let Ok(level) = level_str.parse::<log::LevelFilter>() {
                        log_builder = log_builder.level_for(module.to_string(), level);
                    }
                } else if let Ok(level) = directive.parse::<log::LevelFilter>() {
                    log_builder = log_builder.level(level);
                }
            }
        }
        log_builder.build()
    };

    shell_env::load_shell_environment();

    // Check if onboarding is needed EARLY, before setting up notebook state.
    // If onboarding is needed and no notebook path provided, we'll show the
    // onboarding window instead of creating a notebook.
    let app_settings = settings::load_settings();
    let needs_onboarding = !app_settings.onboarding_completed && notebook_path.is_none();

    // Capture working directory early for untitled notebook project detection.
    // This must happen before Tauri startup, which may change the CWD.
    // Filter out "/" — macOS sets CWD to root when launched from Finder/Dock.
    // Fall back to ~/notebooks (creating it if needed), same as onboarding.
    let working_dir = if notebook_path.is_none() {
        std::env::current_dir()
            .ok()
            .filter(|p| p.parent().is_some())
            .or_else(|| ensure_notebooks_directory().ok())
    } else {
        None
    };

    // Use provided runtime or fall back to user's default from settings
    let runtime = runtime.unwrap_or(app_settings.default_runtime);

    // Try to restore session if no notebook path/id provided and not onboarding
    let restored_session = if notebook_path.is_none() && notebook_id.is_none() && !needs_onboarding
    {
        session::load_session()
    } else {
        None
    };

    // Window registry is always needed for multi-window support
    let window_registry = WindowNotebookRegistry::default();

    // Build the list of ALL notebook windows to create at startup.
    // All windows are created immediately (showing loading UI) and synced
    // with the daemon once it's available — no primary/secondary distinction.
    struct StartupWindow {
        label: String,
        title: String,
        mode: OpenMode,
        saved_scale_factor: Option<f64>,
    }

    let startup_windows: Vec<StartupWindow> = if needs_onboarding {
        info!("[startup] Onboarding needed, skipping notebook state setup");
        Vec::new()
    } else if let Some(ref path) = notebook_path {
        // CLI arg: open a specific notebook
        let title = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Untitled.ipynb")
            .to_string();
        let hash = runt_workspace::worktree_hash(path);
        vec![StartupWindow {
            label: format!("notebook-{}", &hash[..8]),
            title,
            mode: OpenMode::Open { path: path.clone() },
            saved_scale_factor: None,
        }]
    } else if let Some(ref id) = notebook_id {
        // CLI --notebook-id: join an existing untitled notebook by UUID
        vec![StartupWindow {
            label: format!("notebook-{}", &id[..8.min(id.len())]),
            title: "Untitled.ipynb".to_string(),
            mode: OpenMode::Create {
                runtime: runtime.to_string(),
                working_dir: working_dir.clone(),
                notebook_id: Some(id.clone()),
            },
            saved_scale_factor: None,
        }]
    } else if let Some(ref session) = restored_session {
        // Session restore: recreate all windows from the saved session
        session
            .windows
            .iter()
            .filter_map(|ws| {
                let label = session::window_label_for_session(ws);
                let (title, mode) = match (&ws.path, &ws.env_id) {
                    (Some(path), _) if path.exists() => {
                        let title = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("Untitled.ipynb")
                            .to_string();
                        info!("[session] Restoring window from path: {}", path.display());
                        (title, OpenMode::Open { path: path.clone() })
                    }
                    (_, Some(env_id)) => {
                        info!("[session] Restoring untitled window: {}", env_id);
                        (
                            "Untitled.ipynb".to_string(),
                            OpenMode::Create {
                                runtime: ws.runtime.clone(),
                                working_dir: working_dir.clone(),
                                notebook_id: Some(env_id.clone()),
                            },
                        )
                    }
                    _ => {
                        warn!("[session] Skipping session entry with no path or env_id");
                        return None;
                    }
                };
                Some(StartupWindow {
                    label,
                    title,
                    mode,
                    saved_scale_factor: ws.scale_factor,
                })
            })
            .collect()
    } else {
        // Fresh start: create a new untitled notebook
        vec![StartupWindow {
            label: format!("notebook-{}", uuid::Uuid::new_v4()),
            title: "Untitled.ipynb".to_string(),
            mode: OpenMode::Create {
                runtime: runtime.to_string(),
                working_dir: working_dir.clone(),
                notebook_id: None,
            },
            saved_scale_factor: None,
        }]
    };

    // Deduplicate by label — if the same notebook path was open in multiple
    // windows, session restore regenerates the same deterministic label for
    // each, which would crash registry.insert() with "Context already exists".
    let startup_windows = {
        let mut seen = std::collections::HashSet::new();
        startup_windows
            .into_iter()
            .filter(|sw| seen.insert(sw.label.clone()))
            .collect::<Vec<_>>()
    };

    // If session restore yielded no valid windows, fall back to a fresh notebook
    let startup_windows = if !needs_onboarding && startup_windows.is_empty() {
        vec![StartupWindow {
            label: format!("notebook-{}", uuid::Uuid::new_v4()),
            title: "Untitled.ipynb".to_string(),
            mode: OpenMode::Create {
                runtime: runtime.to_string(),
                working_dir: working_dir.clone(),
                notebook_id: None,
            },
            saved_scale_factor: None,
        }]
    } else {
        startup_windows
    };

    // Register all startup window contexts in the registry before setup
    for sw in &startup_windows {
        let placeholder_id = match &sw.mode {
            OpenMode::Open { path } => path
                .canonicalize()
                .unwrap_or_else(|_| path.clone())
                .to_string_lossy()
                .to_string(),
            OpenMode::Create {
                notebook_id: Some(ref id),
                ..
            } => id.clone(),
            OpenMode::Create {
                notebook_id: None, ..
            } => String::new(),
        };
        let context = create_window_context_for_daemon(
            match &sw.mode {
                OpenMode::Open { path } => Some(path.clone()),
                _ => None,
            },
            working_dir.clone(),
            placeholder_id,
            runtime.clone(),
        );
        window_registry
            .insert(&sw.label, context)
            .map_err(anyhow::Error::msg)?;
    }
    log::info!(
        "[startup] Registered {} startup window context(s): {:?}",
        startup_windows.len(),
        startup_windows
            .iter()
            .map(|sw| &sw.label)
            .collect::<Vec<_>>()
    );

    // Guard against concurrent reconnect attempts
    let reconnect_in_progress = ReconnectInProgress(Arc::new(AtomicBool::new(false)));

    // Guard against multiple windows trying to restart daemon simultaneously
    let restart_in_progress = DaemonRestartInProgress(Arc::new(AtomicBool::new(false)));

    // Track last daemon progress status for UI queries (handles race conditions)
    let daemon_status_state = DaemonStatusState(Arc::new(Mutex::new(None)));
    let daemon_status_for_startup = daemon_status_state.0.clone();

    // Daemon sync completion flag - set when notebook sync initialization completes
    // Used to coordinate auto-launch decision with daemon connection status
    let daemon_sync_complete = Arc::new(AtomicBool::new(false));
    let daemon_sync_success = Arc::new(AtomicBool::new(false));

    // Clone for notebook sync initialization
    let registry_for_sync = window_registry.clone();
    let daemon_sync_complete_for_init = daemon_sync_complete.clone();
    let daemon_sync_success_for_init = daemon_sync_success.clone();

    // Clone for auto-launch coordination
    let daemon_sync_complete_for_autolaunch = daemon_sync_complete.clone();
    let daemon_sync_success_for_autolaunch = daemon_sync_success.clone();

    // Deferred file-open URLs — queued when RunEvent::Opened arrives before
    // startup sync completes, preventing prune_stale_entries from removing
    // contexts for startup windows whose Tauri webviews haven't been created yet.
    #[cfg(target_os = "macos")]
    let deferred_open_urls: Arc<Mutex<Vec<tauri::Url>>> = Arc::new(Mutex::new(Vec::new()));

    // Migrate stale "main" window geometry before the window-state plugin loads.
    // Pre-97b0422f versions used a hardcoded "main" label for the first window.
    // The plugin now denylists "main", orphaning its saved geometry. This renames
    // the entry so the new deterministic label picks up the old geometry.
    //
    // Runs unconditionally: the session file may exist even when restored_session
    // is None (e.g., Finder launch, expired session, CLI open). Uses the
    // age-ignoring loader so stale sessions still trigger the one-time migration.
    {
        let session_for_migration = restored_session
            .as_ref()
            .cloned()
            .or_else(session::load_session_ignoring_age);
        if let Some(ref session) = session_for_migration {
            migrate_main_window_state(session);
        }
    }

    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default()
        .plugin(log_plugin)
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_denylist(&["main", "onboarding", "upgrade"])
                .build(),
        );

    #[cfg(feature = "e2e-webdriver")]
    {
        log::info!("[e2e] Registering tauri-plugin-webdriver for E2E testing");
        builder = builder.plugin(tauri_plugin_webdriver::init());
    }

    let app = builder
        .manage(window_registry.clone())
        .manage(reconnect_in_progress)
        .manage(restart_in_progress)
        .manage(daemon_status_state)
        .manage(SyncReadyState::default())
        .invoke_handler(tauri::generate_handler![
            // Notebook file operations
            has_notebook_path,
            mark_notebook_clean,
            save_notebook,
            save_notebook_as,
            get_default_save_directory,
            clone_notebook_to_path,
            open_notebook_in_new_window,
            // Daemon kernel operations (all kernel ops go through daemon)
            launch_kernel_via_daemon,
            execute_cell_via_daemon,
            clear_outputs_via_daemon,
            interrupt_via_daemon,
            shutdown_kernel_via_daemon,
            sync_environment_via_daemon,
            get_daemon_kernel_info,
            is_daemon_connected,
            get_daemon_status,
            get_pool_status,
            get_daemon_queue_state,
            run_all_cells_via_daemon,
            send_comm_via_daemon,
            get_history_via_daemon,
            complete_via_daemon,
            reconnect_to_daemon,
            notify_sync_ready,
            send_frame,
            // App update support
            begin_upgrade,
            get_upgrade_notebook_status,
            abort_kernel_for_upgrade,
            run_upgrade,
            // pyproject.toml discovery
            detect_pyproject,
            get_pyproject_dependencies,
            import_pyproject_dependencies,
            // pixi.toml support
            detect_pixi_toml,
            import_pixi_dependencies,
            // environment.yml support
            detect_environment_yml,
            get_environment_yml_dependencies,
            // Trust verification
            verify_notebook_trust,
            approve_notebook_trust,
            check_typosquats,
            // Deno kernel support
            detect_deno_config,
            // Synced settings (via runtimed Automerge)
            get_synced_settings,
            set_synced_setting,
            open_settings_window,
            // Onboarding
            complete_onboarding,
            // Debug info
            get_git_info,
            get_daemon_info,
            get_blob_port,
            get_username,
            // Feedback
            open_feedback_window,
            get_feedback_system_info,
        ])
        .setup(move |app| {
            let setup_start = std::time::Instant::now();
            log::info!("[startup] App setup starting");

            // Ensure ~/notebooks directory exists for new notebook saves and kernel CWD
            let notebooks_dir = ensure_notebooks_directory()
                .map_err(Box::<dyn std::error::Error>::from)?;
            log::info!("[startup] Notebooks directory: {}", notebooks_dir.display());

            if needs_onboarding {
                // Create dedicated onboarding window with fixed size appropriate for content.
                // Uses the separate onboarding app bundle (not the notebook bundle) to avoid
                // loading notebook hooks that don't apply to the onboarding window.
                let _onboarding_window = tauri::WebviewWindowBuilder::new(
                    app,
                    "onboarding",
                    tauri::WebviewUrl::App("onboarding/index.html".into()),
                )
                .title(format!(
                    "Welcome to {}",
                    runt_workspace::desktop_display_name()
                ))
                .inner_size(1024.0, 768.0)
                .resizable(false)
                .center()
                .build()?;

                log::info!("[startup] Created dedicated onboarding window");
            } else {
                // Create ALL notebook windows immediately. Each shows a loading UI
                // until the daemon is ready and sync completes.
                let startup_username = get_username();
                let startup_init_script = format!(
                    "window.__NTERACT_USERNAME__ = {};",
                    serde_json::to_string(&startup_username)
                        .unwrap_or_else(|_| "\"\"".to_string())
                );
                for sw in &startup_windows {
                    match tauri::WebviewWindowBuilder::new(
                        app,
                        &sw.label,
                        tauri::WebviewUrl::default(),
                    )
                    .title(&sw.title)
                    .initialization_script(&startup_init_script)
                    .inner_size(1100.0, 750.0)
                    .min_inner_size(400.0, 250.0)
                    .resizable(true)
                    .build()
                    {
                        Ok(window) => {
                            log::info!("[startup] Created notebook window: {}", sw.label);
                            correct_window_scale(&window, sw.saved_scale_factor);
                        }
                        Err(e) => log::warn!(
                            "[startup] Failed to create window '{}': {}",
                            sw.label,
                            e
                        ),
                    }
                }
                log::info!(
                    "[startup] All {} startup window(s) created",
                    startup_windows.len()
                );
                refresh_native_menu(
                    app.handle(),
                    app.state::<WindowNotebookRegistry>().inner(),
                );
            }

            // Prevent the app from stealing focus during E2E tests.
            // NSApplicationActivationPolicyAccessory keeps the window visible
            // and functional but won't activate (steal focus) or show in the Dock.
            #[cfg(all(feature = "e2e-webdriver", target_os = "macos"))]
            unsafe {
                use cocoa::appkit::{NSApplication, NSApplicationActivationPolicy};
                use cocoa::base::nil;
                let ns_app = NSApplication::sharedApplication(nil);
                ns_app.setActivationPolicy_(NSApplicationActivationPolicy::NSApplicationActivationPolicyAccessory);
            }

            // Set up native menu bar
            let window_display_names =
                window_menu_display_names(app.handle(), app.state::<WindowNotebookRegistry>().inner());
            let menu = crate::menu::create_menu(app.handle(), &window_display_names)?;
            app.set_menu(menu)?;

            let has_session_to_clear = restored_session.is_some();

            // Ensure runtimed is running (required for daemon-only mode)
            // The daemon provides centralized prewarming across all notebook windows
            let app_for_daemon = app.handle().clone();
            let app_for_sync = app.handle().clone();
            let app_for_notebook_sync = app.handle().clone();
            let registry_for_notebook_sync = registry_for_sync.clone();
            let daemon_status_for_callback = daemon_status_for_startup.clone();
            // Capture for async block - onboarding doesn't need notebook sync
            let skip_notebook_sync = needs_onboarding;
            tauri::async_runtime::spawn(async move {
                // Create progress callback to emit Tauri events for UI feedback
                // Also stores status for later queries (handles race conditions)
                let app_for_progress = app_for_daemon.clone();
                let on_progress = move |progress: runtimed::client::DaemonProgress| {
                    log::info!("[daemon:progress] Emitting event: {:?}", progress);
                    // Store for later queries
                    if let Ok(mut guard) = daemon_status_for_callback.lock() {
                        *guard = Some(progress.clone());
                    }
                    // Emit event for listeners
                    let _ = app_for_progress.emit("daemon:progress", &progress);
                };

                // Use sidecar-based daemon startup (spawns `runtimed install` if needed)
                let daemon_available =
                    match ensure_daemon_via_sidecar(&app_for_daemon, on_progress).await {
                        Ok(endpoint) => {
                            log::info!("[startup] runtimed running at {}", endpoint);
                            true
                        }
                        Err(e) => {
                            // The daemon is required — all kernels run as agent subprocesses.
                            // The failure event was already emitted by ensure_daemon_via_sidecar.
                            log::warn!(
                                "[startup] runtimed not available: {}. Kernel execution will not work until daemon starts.",
                                e
                            );
                            false
                        }
                    };

                // Check if CLI symlinks are current and silently update if stale.
                // This handles app reinstalls, bundle path changes, and channel switches.
                cli_install::ensure_cli_current(&app_for_daemon);

                // Start settings sync subscription (reconnects automatically)
                // Spawn as separate task since it runs forever
                tokio::spawn(run_settings_sync(app_for_sync.clone()));

                // Pool state now syncs via PoolDoc on each notebook connection
                // (frame type 0x06), no separate subscription needed.

                // Initialize notebook sync for all startup windows.
                // Skip during onboarding - the onboarding window doesn't need notebook sync,
                // it just needs daemon progress events.
                if daemon_available && !skip_notebook_sync {
                    let mut any_success = false;
                    for sw in startup_windows {
                        log::info!(
                            "[startup] Initializing sync for '{}' (mode={})",
                            sw.label,
                            match &sw.mode {
                                OpenMode::Open { path } =>
                                    format!("open:{}", path.display()),
                                OpenMode::Create { .. } => "create".into(),
                            }
                        );
                        match (
                            app_for_notebook_sync.get_webview_window(&sw.label),
                            registry_for_notebook_sync.get(&sw.label),
                        ) {
                            (Some(window), Ok(context)) => {
                                let result = match sw.mode {
                                    OpenMode::Open { path } => {
                                        initialize_notebook_sync_open(
                                            window,
                                            path,
                                            context.notebook_sync,
                                            context.sync_generation,
                                            context.notebook_id,
                                        )
                                        .await
                                    }
                                    OpenMode::Create {
                                        runtime: rt,
                                        working_dir: wd,
                                        notebook_id: id_hint,
                                    } => {
                                        initialize_notebook_sync_create(
                                            window,
                                            rt,
                                            wd,
                                            id_hint,
                                            context.notebook_sync,
                                            context.sync_generation,
                                            context.notebook_id,
                                        )
                                        .await
                                    }
                                };
                                match result {
                                    Ok(()) => {
                                        log::info!(
                                            "[startup] Notebook sync initialized for '{}'",
                                            sw.label
                                        );
                                        any_success = true;
                                    }
                                    Err(e) => {
                                        log::warn!(
                                            "[startup] Notebook sync failed for '{}': {}",
                                            sw.label,
                                            e
                                        );
                                    }
                                }
                            }
                            (None, _) => {
                                log::warn!(
                                    "[startup] Window '{}' missing during sync init",
                                    sw.label
                                );
                            }
                            (_, Err(e)) => {
                                log::warn!(
                                    "[startup] Context for '{}' missing: {}",
                                    sw.label,
                                    e
                                );
                            }
                        }
                    }
                    if any_success {
                        daemon_sync_success_for_init.store(true, Ordering::SeqCst);
                    }
                } else if daemon_available && skip_notebook_sync {
                    // Onboarding mode: daemon is available, notebook sync deliberately skipped
                    // Mark as success so autolaunch task doesn't emit error events
                    log::info!("[startup] Skipping notebook sync during onboarding");
                    daemon_sync_success_for_init.store(true, Ordering::SeqCst);
                }
                // Signal that daemon sync attempt is complete (success or failure)
                daemon_sync_complete_for_init.store(true, Ordering::SeqCst);

                // Clear session file after all windows have been synced (or
                // attempted). Keeping it until now allows a retry on next launch
                // if the daemon was unavailable this time.
                if has_session_to_clear {
                    session::clear_session();
                }
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
                        "guidance": runt_workspace::daemon_unavailable_guidance()
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
                        "guidance": runt_workspace::daemon_unavailable_guidance()
                    }));
                }
            });

            Ok(())
        })
        .on_menu_event(|app, event| {
            let menu_id = event.id().as_ref();
            if let Some(window_label) = crate::menu::window_label_for_menu_item_id(menu_id) {
                if let Some(window) = app.get_webview_window(window_label) {
                    if let Err(error) = window.set_focus() {
                        warn!(
                            "[menu] Failed to focus selected window '{}': {}",
                            window_label, error
                        );
                    }
                } else {
                    warn!("[menu] Selected window '{}' is no longer available", window_label);
                }
                return;
            }
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
                    // Emit event to frontend to trigger open dialog when a window exists.
                    // If all windows are closed (macOS app menu still active), fall back
                    // to a native picker so File > Open still works.
                    if let Some(window) = focused_window(app) {
                        let _ = emit_to_label::<_, _, _>(&window, window.label(), "menu:open", ());
                    } else {
                        open_notebook_from_menu_without_window(app, registry.inner());
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
                crate::menu::MENU_INSERT_CODE_CELL => {
                    if let Some(window) = focused_window(app) {
                        let _ =
                            emit_to_label::<_, _, _>(&window, window.label(), "menu:insert-cell", "code");
                    }
                }
                crate::menu::MENU_INSERT_MARKDOWN_CELL => {
                    if let Some(window) = focused_window(app) {
                        let _ = emit_to_label::<_, _, _>(
                            &window,
                            window.label(),
                            "menu:insert-cell",
                            "markdown",
                        );
                    }
                }
                crate::menu::MENU_INSERT_RAW_CELL => {
                    if let Some(window) = focused_window(app) {
                        let _ =
                            emit_to_label::<_, _, _>(&window, window.label(), "menu:insert-cell", "raw");
                    }
                }
                crate::menu::MENU_CLEAR_OUTPUTS => {
                    if let Some(window) = focused_window(app) {
                        let _ =
                            emit_to_label::<_, _, _>(&window, window.label(), "menu:clear-outputs", ());
                    }
                }
                crate::menu::MENU_CLEAR_ALL_OUTPUTS => {
                    if let Some(window) = focused_window(app) {
                        let _ = emit_to_label::<_, _, _>(
                            &window,
                            window.label(),
                            "menu:clear-all-outputs",
                            (),
                        );
                    }
                }
                crate::menu::MENU_CHECK_FOR_UPDATES => {
                    if let Some(window) = focused_window(app) {
                        let _ = emit_to_label::<_, _, _>(
                            &window,
                            window.label(),
                            "menu:check-for-updates",
                            (),
                        );
                    }
                }
                crate::menu::MENU_SETTINGS => {
                    let app_handle = app.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = open_settings_window(app_handle).await {
                            log::error!("[menu] Failed to open settings window: {}", e);
                        }
                    });
                }
                crate::menu::MENU_SEND_FEEDBACK => {
                    let app_handle = app.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = open_feedback_window(app_handle).await {
                            log::error!("[menu] Failed to open feedback window: {}", e);
                        }
                    });
                }
                crate::menu::MENU_INSTALL_CLI => {
                    let app_handle = app.clone();
                    tauri::async_runtime::spawn(async move {
                        // Show confirmation dialog — this requires admin privileges
                        let install_dir = crate::cli_install::SYSTEM_INSTALL_DIR;
                        let confirmed =
                            tauri_plugin_dialog::DialogExt::dialog(&app_handle)
                                .message(format!(
                                    "This will install the CLI commands to {install_dir}, which requires administrator privileges.",
                                ))
                                .title("Install CLI?")
                                .kind(tauri_plugin_dialog::MessageDialogKind::Info)
                                .buttons(tauri_plugin_dialog::MessageDialogButtons::OkCancelCustom(
                                    "Install".to_string(),
                                    "Cancel".to_string(),
                                ))
                                .blocking_show();

                        if !confirmed {
                            return;
                        }

                        let result = tauri::async_runtime::spawn_blocking({
                            let app_handle = app_handle.clone();
                            move || crate::cli_install::install_cli_system(&app_handle)
                        })
                        .await;

                        match result {
                            Ok(Ok(())) => {
                                log::info!("[cli_install] CLI installed successfully");
                                let cli_cmd = runt_workspace::cli_command_name();
                                let nb_cmd = runt_workspace::cli_notebook_alias_name();
                                let success_message = format!(
                                    "The '{cli_cmd}' and '{nb_cmd}' commands have been installed to {install_dir}.\n\nOpen a new terminal and run: {cli_cmd} --help"
                                );
                                let _ = tauri_plugin_dialog::DialogExt::dialog(&app_handle)
                                    .message(success_message)
                                    .title("CLI Installed")
                                    .kind(tauri_plugin_dialog::MessageDialogKind::Info)
                                    .blocking_show();
                            }
                            Ok(Err(e)) => {
                                log::error!("[cli_install] CLI installation failed: {}", e);
                                if e != "Installation cancelled." {
                                    let _ = tauri_plugin_dialog::DialogExt::dialog(&app_handle)
                                        .message(format!("Failed to install CLI: {}", e))
                                        .title("Installation Failed")
                                        .kind(tauri_plugin_dialog::MessageDialogKind::Error)
                                        .blocking_show();
                                }
                            }
                            Err(e) => {
                                log::error!("[cli_install] CLI install task panicked: {}", e);
                            }
                        }
                    });
                }
                crate::menu::MENU_INSTALL_CLAUDE_EXT => {
                    let app_handle = app.clone();
                    match crate::mcpb_install::install_mcpb(&app_handle) {
                        Ok(path) => {
                            log::info!(
                                "[mcpb] Extension opened for installation: {}",
                                path.display()
                            );
                        }
                        Err(e) => {
                            log::error!("[mcpb] Failed to install extension: {}", e);
                            tauri::async_runtime::spawn(async move {
                                let _ = tauri_plugin_dialog::DialogExt::dialog(&app_handle)
                                    .message(format!(
                                        "Failed to create Claude extension: {}\n\n\
                                         Make sure Claude Desktop is installed.",
                                        e
                                    ))
                                    .title("Extension Install Failed")
                                    .kind(tauri_plugin_dialog::MessageDialogKind::Error)
                                    .blocking_show();
                            });
                        }
                    }
                }
                crate::menu::MENU_OPEN_SAMPLE => {
                    let sample = &crate::menu::BUNDLED_SAMPLE_NOTEBOOK;
                    if let Err(e) = open_bundled_sample_notebook(app, registry.inner(), sample) {
                        log::error!(
                            "[sample_notebooks] Failed to open sample {}: {}",
                            sample.file_name,
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
                _ => {
                }
            }
        })
        .build(tauri::generate_context!())
        .map_err(|e| anyhow::anyhow!("Tauri build error: {}", e))?;

    #[cfg(target_os = "macos")]
    let registry_for_open = window_registry.clone();
    #[cfg(target_os = "macos")]
    let daemon_sync_complete_for_open = daemon_sync_complete.clone();
    #[cfg(target_os = "macos")]
    let deferred_urls_for_open = deferred_open_urls.clone();
    let registry_for_session = window_registry.clone();
    let registry_for_exit_session = window_registry.clone();
    let registry_for_window_close = window_registry.clone();
    let app_quitting = Arc::new(AtomicBool::new(false));
    app.run(move |app_handle, event| {
        // Drain deferred file-open URLs once startup sync is complete.
        // These were queued by RunEvent::Opened events that arrived before
        // startup windows were fully created and synced.
        #[cfg(target_os = "macos")]
        if daemon_sync_complete_for_open.load(Ordering::SeqCst) {
            if let Ok(mut q) = deferred_urls_for_open.lock() {
                if !q.is_empty() {
                    let urls: Vec<tauri::Url> = std::mem::take(&mut *q);
                    drop(q);
                    log::info!(
                        "[file-open] Processing {} deferred open event(s)",
                        urls.len()
                    );
                    registry_for_open.prune_stale_entries(app_handle);
                    for url in &urls {
                        handle_open_url(app_handle, &registry_for_open, url);
                    }
                }
            }
        }

        // Save session at ExitRequested — before windows are destroyed.
        // WindowEvent::Destroyed removes registry entries, so by RunEvent::Exit
        // the registry is empty and save_session() would no-op.
        #[cfg(target_os = "macos")]
        if let RunEvent::ExitRequested { code, api, .. } = &event {
            if code.is_none() && app_handle.webview_windows().is_empty() {
                // Last window closed via X — keep app alive for dock (macOS)
                log::info!("[app] Preventing exit after closing last window (macOS)");
                clear_all_notebook_sync_handles(
                    &registry_for_window_close,
                    "macos last-window close",
                );
                api.prevent_exit();
            } else {
                // Real quit (Cmd+Q or code-initiated). Save now while windows are alive.
                app_quitting.store(true, Ordering::SeqCst);
                log::info!("[session] Saving session before windows are destroyed");
                registry_for_exit_session.prune_stale_entries(app_handle);
                if let Err(e) = session::save_session(&registry_for_exit_session, app_handle) {
                    log::error!("[session] Failed to save session on exit: {}", e);
                }
            }
        }

        #[cfg(not(target_os = "macos"))]
        if let RunEvent::ExitRequested { .. } = &event {
            app_quitting.store(true, Ordering::SeqCst);
            log::info!("[session] Saving session before windows are destroyed");
            registry_for_exit_session.prune_stale_entries(app_handle);
            if let Err(e) = session::save_session(&registry_for_exit_session, app_handle) {
                log::error!("[session] Failed to save session on exit: {}", e);
            }
        }

        // Clean up registry entries when windows are destroyed
        if let RunEvent::WindowEvent {
            label,
            event: WindowEvent::Destroyed,
            ..
        } = &event
        {
            if let Ok(mut contexts) = registry_for_window_close.contexts.lock() {
                let closed_handle = contexts.remove(label).map(|context| {
                    log::info!(
                        "[window] Removed registry entry for closed window: {}",
                        label
                    );
                    context.notebook_sync
                });

                if let Some(notebook_sync) = closed_handle {
                    clear_notebook_sync_handles(
                        vec![(label.clone(), notebook_sync)],
                        "window destroyed",
                    );
                }
            }
            if !app_quitting.load(Ordering::SeqCst) {
                refresh_native_menu(app_handle, &registry_for_window_close);
            }
        }

        // Fallback session save. ExitRequested (above) is the primary save point;
        // by this time Destroyed events have usually emptied the registry, so
        // save_session returns early without overwriting.
        if let RunEvent::Exit = &event {
            log::info!("[session] App exiting, saving session (fallback)...");
            if let Err(e) = session::save_session(&registry_for_session, app_handle) {
                log::error!("[session] Failed to save session: {}", e);
            }
        }

        // Handle file associations (macOS only).
        // During startup, incoming Apple Events are deferred to prevent
        // prune_stale_entries from removing contexts for startup windows
        // whose Tauri webviews haven't been created yet.
        #[cfg(target_os = "macos")]
        if let RunEvent::Opened { urls } = &event {
            log::info!(
                "[file-open] RunEvent::Opened with {} URL(s): {:?}",
                urls.len(),
                urls.iter()
                    .filter_map(|u| u.to_file_path().ok())
                    .collect::<Vec<_>>()
            );
            if !daemon_sync_complete_for_open.load(Ordering::SeqCst) {
                log::info!(
                    "[file-open] Deferring {} open event(s) until startup completes",
                    urls.len()
                );
                if let Ok(mut q) = deferred_urls_for_open.lock() {
                    q.extend(urls.iter().cloned());
                }
            } else {
                registry_for_open.prune_stale_entries(app_handle);
                for url in urls {
                    handle_open_url(app_handle, &registry_for_open, url);
                }
            }
        }

        #[cfg(target_os = "macos")]
        if let RunEvent::Reopen {
            has_visible_windows,
            ..
        } = &event
        {
            match reopen_action(*has_visible_windows, app_handle.webview_windows().len()) {
                Some(ReopenAction::RestoreWindow) => {
                    let window = app_handle.webview_windows().into_values().next();

                    if let Some(window) = window {
                        if let Err(error) = window.show() {
                            warn!(
                                "[app] Failed to show reopened window '{}': {}",
                                window.label(),
                                error
                            );
                        }
                        if let Err(error) = window.unminimize() {
                            warn!(
                                "[app] Failed to unminimize reopened window '{}': {}",
                                window.label(),
                                error
                            );
                        }
                        if let Err(error) = window.set_focus() {
                            warn!(
                                "[app] Failed to focus reopened window '{}': {}",
                                window.label(),
                                error
                            );
                        }
                    }
                }
                Some(ReopenAction::SpawnNotebook) => {
                    let runtime = settings::load_settings().default_runtime;
                    if let Err(error) = spawn_new_notebook(app_handle, &registry_for_open, runtime)
                    {
                        warn!(
                            "[app] Failed to create notebook window on reopen: {}",
                            error
                        );
                    }
                }
                None => {}
            }
        }
    });

    Ok(())
}
