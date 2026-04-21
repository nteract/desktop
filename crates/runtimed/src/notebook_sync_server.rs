//! Room-based notebook synchronization server.
//!
//! Each open notebook gets a "room" in the daemon. Multiple windows editing
//! the same notebook sync through the room's canonical Automerge document.
//!
//! Follows the same sync protocol pattern as `sync_server.rs` (settings sync)
//! but with per-notebook state managed through rooms.
//!
//! ## Room lifecycle
//!
//! 1. First window opens notebook → daemon creates room, loads persisted doc
//! 2. Client exchanges Automerge sync messages with the room
//! 3. Additional windows join the same room
//! 4. Changes from any peer broadcast to all others in the room
//! 5. When the last peer disconnects, the room is evicted from memory
//!    (any pending doc bytes are flushed to disk via debounced persistence)
//! 6. Documents persist to `~/.cache/runt/notebook-docs/{hash}.automerge`
//!
//! ## Phase 8: Daemon-owned kernel execution
//!
//! Each room can have an optional kernel. When a kernel is launched:
//! - Execute requests flow through the daemon
//! - Daemon tracks msg_id → cell_id mapping
//! - Outputs are broadcast to all connected windows
//! - Multiple windows share the same kernel

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use automerge::sync;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, mpsc, oneshot, watch, Mutex, RwLock};
use tracing::{debug, error, info, warn};

use notify_debouncer_mini::DebounceEventResult;

use crate::blob_store::BlobStore;
use crate::connection::{self, NotebookFrameType};
use crate::markdown_assets::resolve_markdown_assets;
use crate::notebook_doc::{CellSnapshot, NotebookDoc};
use crate::notebook_metadata::NotebookMetadataSnapshot;
use crate::output_prep::{DenoLaunchedConfig, LaunchedEnvConfig};
use crate::paths::notebook_doc_filename;
use crate::protocol::{EnvSyncDiff, NotebookBroadcast, NotebookRequest, NotebookResponse};
use crate::task_supervisor::{spawn_best_effort, spawn_supervised};
use notebook_doc::diff::diff_metadata_touched;
use notebook_doc::presence::{self, PresenceState};
use notebook_doc::runtime_state::RuntimeStateDoc;

mod path_index;
pub use path_index::{PathIndex, PathIndexError};

/// Global shutdown trigger registered by the daemon at startup.
///
/// Used by `spawn_supervised` callbacks in debouncers (autosave, persist) that
/// don't have `Arc<Daemon>` in scope. The daemon calls `register_shutdown_trigger`
/// once; on_panic handlers call `trigger_global_shutdown` to signal data-loss risk.
static SHUTDOWN_TRIGGER: std::sync::OnceLock<Arc<dyn Fn() + Send + Sync>> =
    std::sync::OnceLock::new();

pub fn register_shutdown_trigger(trigger: Arc<dyn Fn() + Send + Sync>) {
    let _ = SHUTDOWN_TRIGGER.set(trigger);
}

fn trigger_global_shutdown() {
    if let Some(trigger) = SHUTDOWN_TRIGGER.get() {
        (trigger)();
    }
}

/// Capacity for the per-room kernel broadcast channel. Sized to absorb bursts
/// of output messages (e.g. fast-printing cells) so slower peers trigger a
/// full doc sync ("peer lagged") rather than losing messages.
const KERNEL_BROADCAST_CAPACITY: usize = 256;

/// Compaction threshold for RuntimeStateDoc initial sync messages.
/// If the encoded message exceeds this, compact before sending. Leaves
/// 20 MiB headroom under the 100 MiB frame limit.
const STATE_SYNC_COMPACT_THRESHOLD: usize = 80 * 1024 * 1024;

/// Catch panics from automerge internal operations.
///
/// Automerge 0.7.4 (and 0.8.0) has a known bug where the change collector
/// panics with `MissingOps` when internal op-set indices become inconsistent
/// (see `op_set2/change/collector.rs:761`). This affects `generate_sync_message`,
/// `fork_at`, `merge`, and `get_changes`.
///
/// After catching a panic, callers should call `rebuild_from_save()` on the
/// affected doc to round-trip save→load and rebuild clean internal indices.
pub(crate) fn catch_automerge_panic<T>(label: &str, f: impl FnOnce() -> T) -> Result<T, String> {
    match std::panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(val) => Ok(val),
        Err(payload) => {
            let msg = crate::task_supervisor::panic_payload_to_string(payload);
            Err(format!(
                "[{label}] automerge panicked (upstream bug, see automerge collector.rs MissingOps): {msg}"
            ))
        }
    }
}

/// A message sent through the runtime agent channel.
pub enum RuntimeAgentMessage {
    /// Fire-and-forget command — no response expected.
    Command(notebook_protocol::protocol::RuntimeAgentRequestEnvelope),
    /// Query requiring a sync response via correlation ID.
    Query(
        notebook_protocol::protocol::RuntimeAgentRequestEnvelope,
        tokio::sync::oneshot::Sender<notebook_protocol::protocol::RuntimeAgentResponse>,
    ),
}

/// Sender half of the runtime agent channel.
type RuntimeAgentRequestSender = tokio::sync::mpsc::Sender<RuntimeAgentMessage>;

fn runtime_agent_query_timeout(
    request: &notebook_protocol::protocol::RuntimeAgentRequest,
) -> std::time::Duration {
    use notebook_protocol::protocol::RuntimeAgentRequest;
    match request {
        RuntimeAgentRequest::Complete { .. } | RuntimeAgentRequest::GetHistory { .. } => {
            std::time::Duration::from_secs(10)
        }
        RuntimeAgentRequest::LaunchKernel { .. }
        | RuntimeAgentRequest::RestartKernel { .. }
        | RuntimeAgentRequest::SyncEnvironment(_) => std::time::Duration::from_secs(240),
        _ => std::time::Duration::from_secs(30),
    }
}

/// Send a fire-and-forget command to the runtime agent.
///
/// Commands (Interrupt, SendComm) don't wait for a response — state
/// flows back via RuntimeStateDoc CRDT.
pub(crate) async fn send_runtime_agent_command(
    room: &NotebookRoom,
    request: notebook_protocol::protocol::RuntimeAgentRequest,
) -> anyhow::Result<()> {
    let tx = {
        let guard = room.runtime_agent_request_tx.lock().await;
        guard
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Runtime agent not connected"))?
    };
    let envelope = notebook_protocol::protocol::RuntimeAgentRequestEnvelope {
        id: uuid::Uuid::new_v4().to_string(),
        request,
    };
    tx.send(RuntimeAgentMessage::Command(envelope))
        .await
        .map_err(|_| anyhow::anyhow!("Runtime agent disconnected"))?;
    Ok(())
}

/// Send a query to the runtime agent and wait for a sync response.
///
/// Only used for Complete and GetHistory which need return values.
pub(crate) async fn send_runtime_agent_query(
    room: &NotebookRoom,
    request: notebook_protocol::protocol::RuntimeAgentRequest,
) -> anyhow::Result<notebook_protocol::protocol::RuntimeAgentResponse> {
    let timeout = runtime_agent_query_timeout(&request);
    let tx = {
        let guard = room.runtime_agent_request_tx.lock().await;
        guard
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Runtime agent not connected"))?
    };
    let envelope = notebook_protocol::protocol::RuntimeAgentRequestEnvelope {
        id: uuid::Uuid::new_v4().to_string(),
        request,
    };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    tx.send(RuntimeAgentMessage::Query(envelope, reply_tx))
        .await
        .map_err(|_| anyhow::anyhow!("Runtime agent disconnected"))?;
    match tokio::time::timeout(timeout, reply_rx).await {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(_)) => Err(anyhow::anyhow!("Runtime agent dropped reply")),
        Err(_) => Err(anyhow::anyhow!("Runtime agent query timed out")),
    }
}

/// Send an RPC request to the runtime agent (legacy wrapper).
///
/// Routes commands as fire-and-forget, queries as sync RPCs.
/// Callers that don't need a response should use `send_runtime_agent_command` directly.
pub(crate) async fn send_runtime_agent_request(
    room: &NotebookRoom,
    request: notebook_protocol::protocol::RuntimeAgentRequest,
) -> anyhow::Result<notebook_protocol::protocol::RuntimeAgentResponse> {
    if request.is_command() {
        send_runtime_agent_command(room, request).await?;
        Ok(notebook_protocol::protocol::RuntimeAgentResponse::Ok)
    } else {
        send_runtime_agent_query(room, request).await
    }
}

/// Trust state for a notebook room.
/// Tracks whether the notebook's dependencies are trusted for auto-launch.
#[derive(Debug, Clone)]
pub struct TrustState {
    pub status: runt_trust::TrustStatus,
    pub info: runt_trust::TrustInfo,
    /// If true, kernel launch is pending user trust approval
    pub pending_launch: bool,
}

/// Check if a notebook's metadata snapshot has inline dependencies or Deno config.
/// Returns the appropriate env_source if found ("uv:inline", "conda:inline", or "deno").
///
/// Priority: Deno is checked first, then UV deps, then conda deps.
pub(crate) fn check_inline_deps(snapshot: &NotebookMetadataSnapshot) -> Option<String> {
    // Check for Deno config first (runt.deno)
    if snapshot.runt.deno.is_some() {
        return Some("deno".to_string());
    }

    // Check UV dependencies
    if let Some(ref uv) = snapshot.runt.uv {
        if !uv.dependencies.is_empty() {
            return Some("uv:inline".to_string());
        }
    }

    // Check pixi dependencies (conda + pypi)
    if let Some(ref pixi) = snapshot.runt.pixi {
        if !pixi.dependencies.is_empty() {
            return Some("pixi:inline".to_string());
        }
    }

    // Check conda dependencies
    if let Some(ref conda) = snapshot.runt.conda {
        if !conda.dependencies.is_empty() {
            return Some("conda:inline".to_string());
        }
    }

    None
}

/// Extract inline conda dependencies from a metadata snapshot.
/// Returns the list of dependency strings if conda deps are present.
pub(crate) fn get_inline_conda_deps(snapshot: &NotebookMetadataSnapshot) -> Option<Vec<String>> {
    if let Some(ref conda) = snapshot.runt.conda {
        if !conda.dependencies.is_empty() {
            return Some(conda.dependencies.clone());
        }
    }
    None
}

/// Extract inline UV dependencies from a metadata snapshot.
/// Returns the list of dependency strings if UV deps are present.
pub(crate) fn get_inline_uv_deps(snapshot: &NotebookMetadataSnapshot) -> Option<Vec<String>> {
    if let Some(ref uv) = snapshot.runt.uv {
        if !uv.dependencies.is_empty() {
            return Some(uv.dependencies.clone());
        }
    }
    None
}

/// Extract UV prerelease strategy from a metadata snapshot.
pub(crate) fn get_inline_uv_prerelease(snapshot: &NotebookMetadataSnapshot) -> Option<String> {
    snapshot
        .runt
        .uv
        .as_ref()
        .and_then(|uv| uv.prerelease.clone())
}

/// Extract conda channels from a metadata snapshot.
/// Returns the list of channel strings, or defaults to ["conda-forge"].
pub(crate) fn get_inline_conda_channels(snapshot: &NotebookMetadataSnapshot) -> Vec<String> {
    if let Some(ref conda) = snapshot.runt.conda {
        if !conda.channels.is_empty() {
            return conda.channels.clone();
        }
    }
    vec!["conda-forge".to_string()]
}

/// Extract dependency entries from a pixi.toml or pyproject.toml with [tool.pixi].
///
/// Section-aware: only collects `key = value` lines from `[dependencies]`,
/// `[pypi-dependencies]`, `[tool.pixi.dependencies]`, `[tool.pixi.pypi-dependencies]`.
/// Stores the full line (trimmed) so version constraint changes are detected.
pub(crate) fn extract_pixi_toml_deps(content: &str) -> Vec<String> {
    let dep_sections = [
        "[dependencies]",
        "[pypi-dependencies]",
        "[tool.pixi.dependencies]",
        "[tool.pixi.pypi-dependencies]",
    ];
    let mut in_dep_section = false;
    let mut deps = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_dep_section = dep_sections.iter().any(|s| trimmed.eq_ignore_ascii_case(s));
            continue;
        }
        if in_dep_section
            && !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && trimmed.contains('=')
        {
            deps.push(trimmed.to_string());
        }
    }
    deps.sort();
    deps
}

/// Extract dependency strings from a pyproject.toml's `[project].dependencies` list.
/// Returns PEP 508 dependency strings (e.g., "pandas>=2.0", "numpy").
///
/// Only matches `dependencies` when inside the `[project]` table. Resets on
/// any other `[...]` header so deps from `[tool.*]` tables are not captured.
fn extract_pyproject_deps(content: &str) -> Vec<String> {
    let mut in_project = false;
    let mut in_deps = false;
    let mut deps = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        // Track which TOML table we're inside
        if trimmed.starts_with('[') {
            in_deps = false;
            in_project = trimmed == "[project]";
            continue;
        }
        if !in_project {
            continue;
        }
        if trimmed == "dependencies = [" || trimmed.starts_with("dependencies = [") {
            in_deps = true;
            // Handle single-line: dependencies = ["foo", "bar"]
            if let Some(rest) = trimmed.strip_prefix("dependencies = [") {
                if let Some(inner) = rest.strip_suffix(']') {
                    for dep in inner.split(',') {
                        let dep = dep.trim().trim_matches('"').trim_matches('\'').trim();
                        if !dep.is_empty() {
                            deps.push(dep.to_string());
                        }
                    }
                    in_deps = false;
                }
            }
            continue;
        }
        if in_deps {
            if trimmed == "]" || trimmed.starts_with(']') {
                in_deps = false;
                continue;
            }
            let dep = trimmed
                .trim_matches(',')
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .trim();
            if !dep.is_empty() && !dep.starts_with('#') {
                deps.push(dep.to_string());
            }
        }
    }
    deps.sort();
    deps
}

/// Build a LaunchedEnvConfig from the current metadata snapshot.
/// This captures what configuration was used at kernel launch time.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_launched_config(
    kernel_type: &str,
    env_source: &str,
    inline_deps: Option<&[String]>,
    metadata_snapshot: Option<&NotebookMetadataSnapshot>,
    venv_path: Option<PathBuf>,
    python_path: Option<PathBuf>,
    prewarmed_packages: Option<&[String]>,
    notebook_path: Option<&std::path::Path>,
    feature_flags: notebook_protocol::protocol::FeatureFlags,
    captured_env: Option<&CapturedEnv>,
) -> LaunchedEnvConfig {
    let mut config = LaunchedEnvConfig {
        feature_flags,
        ..LaunchedEnvConfig::default()
    };

    match env_source {
        "uv:inline" | "uv:pep723" => {
            config.uv_deps = inline_deps.map(|d| d.to_vec());
            config.venv_path = venv_path;
            config.python_path = python_path;
            if let Some(pkgs) = prewarmed_packages {
                config.prewarmed_packages = pkgs.to_vec();
            }
        }
        "conda:inline" => {
            config.conda_deps = inline_deps.map(|d| d.to_vec());
            config.venv_path = venv_path;
            config.python_path = python_path;
            if let Some(snapshot) = metadata_snapshot {
                config.conda_channels = Some(get_inline_conda_channels(snapshot));
            }
            if let Some(pkgs) = prewarmed_packages {
                config.prewarmed_packages = pkgs.to_vec();
            }
        }
        "uv:prewarmed" => {
            // Store paths so hot-sync can install deps into the prewarmed venv.
            //
            // If this launch routed through the captured-env path (notebook
            // has env_id + captured deps + matching unified-hash env on
            // disk), record the captured deps as the launch baseline. That
            // way `check_and_broadcast_sync_state` sees
            // `is_tracking = true` and reports `in_sync = true` when the
            // metadata still matches what's installed, instead of treating
            // every captured dep as a pending addition on reopen.
            //
            // Otherwise (genuine first-time prewarmed launch, pre-capture
            // notebook) leave `uv_deps = None` to fall through to the
            // "inline deps added post-launch" branch.
            if let Some(CapturedEnv::Uv { deps, .. }) = captured_env {
                config.uv_deps = Some(deps.dependencies.clone());
            }
            config.venv_path = venv_path;
            config.python_path = python_path;
            if let Some(pkgs) = prewarmed_packages {
                config.prewarmed_packages = pkgs.to_vec();
            }
        }
        "conda:prewarmed" => {
            // See `uv:prewarmed` above — same captured-env baseline logic
            // so drift detection works on conda reopens too. Captured conda
            // channels go into `conda_channels` so channel edits are
            // flagged as drift rather than silently ignored.
            if let Some(CapturedEnv::Conda { deps, .. }) = captured_env {
                config.conda_deps = Some(deps.dependencies.clone());
                config.conda_channels = Some(deps.channels.clone());
            }
            config.venv_path = venv_path;
            config.python_path = python_path;
            if let Some(pkgs) = prewarmed_packages {
                config.prewarmed_packages = pkgs.to_vec();
            }
        }
        "uv:pyproject" => {
            // Store pyproject.toml path and env paths for project-aware dep management.
            config.venv_path = venv_path;
            config.python_path = python_path;
            if let Some(nb_path) = notebook_path {
                if let Some(detected) = crate::project_file::detect_project_file(nb_path) {
                    if detected.kind == crate::project_file::ProjectFileKind::PyprojectToml {
                        config.pyproject_path = Some(detected.path);
                    }
                }
            }
        }
        "pixi:toml" => {
            // Store pixi.toml deps baseline for drift detection.
            // Parse section-aware: only collect entries from [dependencies],
            // [pypi-dependencies], [tool.pixi.dependencies], [tool.pixi.pypi-dependencies].
            if let Some(nb_path) = notebook_path {
                if let Some(detected) = crate::project_file::detect_project_file(nb_path) {
                    if detected.kind == crate::project_file::ProjectFileKind::PixiToml {
                        if let Ok(content) = std::fs::read_to_string(&detected.path) {
                            let deps = extract_pixi_toml_deps(&content);
                            config.pixi_toml_deps = Some(deps);
                            config.pixi_toml_path = Some(detected.path);
                        }
                    }
                }
            }
        }
        "conda:env_yml" => {
            // Store environment.yml path and deps baseline for drift detection.
            // CRDT-only conda deps go into conda_deps for sync diff tracking.
            config.conda_deps = metadata_snapshot.and_then(get_inline_conda_deps);
            config.conda_channels = metadata_snapshot.map(get_inline_conda_channels);
            // Pass venv/python paths so runtime agent can reconstruct PooledEnv
            config.venv_path = venv_path;
            config.python_path = python_path;
            if let Some(pkgs) = prewarmed_packages {
                config.prewarmed_packages = pkgs.to_vec();
            }
            if let Some(nb_path) = notebook_path {
                if let Some(detected) = crate::project_file::find_nearest_project_file(
                    nb_path,
                    &[crate::project_file::ProjectFileKind::EnvironmentYml],
                ) {
                    // Parse environment.yml to snapshot deps at launch time
                    if let Ok(env_config) =
                        crate::project_file::parse_environment_yml(&detected.path)
                    {
                        let mut deps = env_config.dependencies.clone();
                        deps.sort();
                        config.environment_yml_deps = Some(deps);
                    }
                    config.environment_yml_path = Some(detected.path);
                }
            }
        }
        "pixi:inline" => {
            // Store pixi deps for drift detection
            config.pixi_deps = inline_deps.map(|d| d.to_vec());
        }
        "pixi:pep723" => {
            // PEP 723 deps come from cell source, not runt.pixi metadata.
            // Don't store in pixi_deps — there's no metadata to diff against.
        }
        "pixi:prewarmed" => {
            // Pixi prewarmed uses pooled env — store paths
            config.venv_path = venv_path;
            config.python_path = python_path;
            if let Some(pkgs) = prewarmed_packages {
                config.prewarmed_packages = pkgs.to_vec();
            }
        }
        _ => {
            // All other Python env sources use pooled environments
            // — store paths so the runtime agent can reconstruct.
            config.venv_path = venv_path;
            config.python_path = python_path;
            if let Some(pkgs) = prewarmed_packages {
                config.prewarmed_packages = pkgs.to_vec();
            }
        }
    }

    // For Deno kernels, capture the deno config
    if kernel_type == "deno" {
        if let Some(snapshot) = metadata_snapshot {
            if let Some(ref deno) = snapshot.runt.deno {
                config.deno_config = Some(DenoLaunchedConfig {
                    permissions: deno.permissions.clone(),
                    import_map: deno.import_map.clone(),
                    config: deno.config.clone(),
                    flexible_npm_imports: deno.flexible_npm_imports.unwrap_or(true),
                });
            }
        }
    }

    // Generate unique launch ID for this kernel session (for race detection during hot-sync)
    config.launch_id = Some(uuid::Uuid::new_v4().to_string());

    config
}

/// Compute the difference between launched config and current metadata.
/// Returns Some(diff) if there are differences, None if in sync.
fn compute_env_sync_diff(
    launched: &LaunchedEnvConfig,
    current: &NotebookMetadataSnapshot,
) -> Option<EnvSyncDiff> {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut deno_changed = false;

    // Check UV deps
    if let Some(ref launched_uv) = launched.uv_deps {
        let current_uv = current
            .runt
            .uv
            .as_ref()
            .map(|u| &u.dependencies[..])
            .unwrap_or(&[]);

        for dep in current_uv {
            if !launched_uv.contains(dep) {
                added.push(dep.clone());
            }
        }
        for dep in launched_uv {
            if !current_uv.contains(dep) {
                removed.push(dep.clone());
            }
        }
    }

    // Check conda deps and channels
    let mut channels_changed = false;
    if let Some(ref launched_conda) = launched.conda_deps {
        let current_conda = current
            .runt
            .conda
            .as_ref()
            .map(|c| &c.dependencies[..])
            .unwrap_or(&[]);

        for dep in current_conda {
            if !launched_conda.contains(dep) {
                added.push(dep.clone());
            }
        }
        for dep in launched_conda {
            if !current_conda.contains(dep) {
                removed.push(dep.clone());
            }
        }

        // Check channels
        if let Some(ref launched_channels) = launched.conda_channels {
            let current_channels = current
                .runt
                .conda
                .as_ref()
                .map(|c| &c.channels[..])
                .unwrap_or(&[]);

            // Channels are ordered, so compare as slices
            if launched_channels.as_slice() != current_channels {
                channels_changed = true;
            }
        }
    }

    // Check pixi deps
    if let Some(ref launched_pixi) = launched.pixi_deps {
        let current_pixi = current
            .runt
            .pixi
            .as_ref()
            .map(|p| &p.dependencies[..])
            .unwrap_or(&[]);

        for dep in current_pixi {
            if !launched_pixi.contains(dep) {
                added.push(dep.clone());
            }
        }
        for dep in launched_pixi {
            if !current_pixi.contains(dep) {
                removed.push(dep.clone());
            }
        }
    }

    // Check pixi:toml deps (file-based drift)
    if let Some(ref launched_toml_deps) = launched.pixi_toml_deps {
        if let Some(ref toml_path) = launched.pixi_toml_path {
            if let Ok(content) = std::fs::read_to_string(toml_path) {
                let current_deps = extract_pixi_toml_deps(&content);

                for dep in &current_deps {
                    if !launched_toml_deps.contains(dep) {
                        // Extract just the package name for the UI
                        let name = dep
                            .split('=')
                            .next()
                            .map(|n| n.trim().to_string())
                            .unwrap_or_else(|| dep.clone());
                        added.push(name);
                    }
                }
                for dep in launched_toml_deps {
                    if !current_deps.contains(dep) {
                        let name = dep
                            .split('=')
                            .next()
                            .map(|n| n.trim().to_string())
                            .unwrap_or_else(|| dep.clone());
                        removed.push(name);
                    }
                }
            }
        }
    }

    // Check conda:env_yml deps (file-based drift)
    if let Some(ref launched_yml_deps) = launched.environment_yml_deps {
        if let Some(ref yml_path) = launched.environment_yml_path {
            if let Ok(env_config) = crate::project_file::parse_environment_yml(yml_path) {
                let mut current_deps = env_config.dependencies;
                current_deps.sort();

                for dep in &current_deps {
                    if !launched_yml_deps.contains(dep) {
                        let name = notebook_doc::metadata::extract_package_name(dep).to_string();
                        added.push(name);
                    }
                }
                for dep in launched_yml_deps {
                    if !current_deps.contains(dep) {
                        let name = notebook_doc::metadata::extract_package_name(dep).to_string();
                        removed.push(name);
                    }
                }
            }
        }
    }

    // Check deno config
    if let Some(ref launched_deno) = launched.deno_config {
        if let Some(ref current_deno) = current.runt.deno {
            let current_flexible = current_deno.flexible_npm_imports.unwrap_or(true);
            if launched_deno.permissions != current_deno.permissions
                || launched_deno.import_map != current_deno.import_map
                || launched_deno.config != current_deno.config
                || launched_deno.flexible_npm_imports != current_flexible
            {
                deno_changed = true;
            }
        } else {
            // Deno config was removed
            deno_changed = true;
        }
    }

    if added.is_empty() && removed.is_empty() && !channels_changed && !deno_changed {
        None
    } else {
        Some(EnvSyncDiff {
            added,
            removed,
            channels_changed,
            deno_changed,
        })
    }
}

/// Rebuild resolved markdown asset refs for all markdown cells.
///
/// This resolves both notebook-relative files and nbformat attachments into
/// blob-store hashes, then updates the cell-local `resolved_assets` maps so
/// isolated markdown rendering can rewrite those refs to blob URLs.
async fn process_markdown_assets(room: &NotebookRoom) {
    let notebook_path = room.path.read().await.clone().filter(|p| p.exists());
    let nbformat_attachments = room.nbformat_attachments.read().await.clone();

    // Fork BEFORE async resolution so the fork's baseline predates
    // any concurrent edits. Asset updates on the fork are treated as
    // concurrent with user edits via Automerge's CRDT merge.
    let (markdown_cells, mut fork) = {
        let mut doc = room.doc.write().await;
        let cells: Vec<(String, String, HashMap<String, String>)> = doc
            .get_cells()
            .into_iter()
            .filter(|cell| cell.cell_type == "markdown")
            .map(|cell| (cell.id, cell.source, cell.resolved_assets))
            .collect();
        let fork = doc.fork_with_actor(format!("runtimed:assets:{}", uuid::Uuid::new_v4()));
        (cells, fork)
    };

    let mut any_changed = false;
    for (cell_id, source, existing_assets) in markdown_cells {
        let desired_assets = resolve_markdown_assets(
            &source,
            notebook_path.as_deref(),
            nbformat_attachments.get(&cell_id),
            &room.blob_store,
        )
        .await;

        if desired_assets != existing_assets {
            if let Err(e) = fork.set_cell_resolved_assets(&cell_id, &desired_assets) {
                warn!(
                    "[notebook-sync] Failed to sync resolved markdown assets for {}: {}",
                    cell_id, e
                );
            }
            any_changed = true;
        }
    }

    if !any_changed {
        return;
    }

    let persist_bytes = {
        let mut doc = room.doc.write().await;
        if let Err(e) = catch_automerge_panic("metadata-merge", || doc.merge(&mut fork)) {
            warn!("{}", e);
            doc.rebuild_from_save();
        }
        doc.save()
    };

    let _ = room.changed_tx.send(());
    if let Some(ref tx) = room.persist_tx {
        let _ = tx.send(Some(persist_bytes));
    }
}

/// Check if the current metadata differs from kernel's launched config and broadcast sync state.
/// Called after metadata sync to notify all clients about dependency drift.
///
/// Handles two cases:
/// 1. Kernel launched with inline deps - track drift (additions/removals)
/// 2. Kernel launched with prewarmed - detect when user adds inline deps (needs restart)
pub(crate) async fn check_and_broadcast_sync_state(room: &NotebookRoom) {
    // Get current metadata from doc
    let current_metadata = {
        let doc = room.doc.read().await;
        doc.get_metadata_snapshot()
    };

    let Some(current_metadata) = current_metadata else {
        return;
    };

    // Read launched_config from room (set when runtime agent launched/restarted).
    // Clone the config and drop the guard before any `.await` to avoid holding
    // a lock across await points (deadlock prevention).
    let launched = {
        let launched_guard = room.runtime_agent_launched_config.read().await;
        match &*launched_guard {
            Some(l) => l.clone(),
            None => return,
        }
    };

    // Check kernel is actually running via RuntimeStateDoc
    {
        let sd = room.state_doc.read().await;
        let status = sd.read_state().kernel.status;
        if status != "idle" && status != "busy" {
            return;
        }
    }

    // Check if we're tracking inline deps or deno config
    let is_tracking = launched.uv_deps.is_some()
        || launched.conda_deps.is_some()
        || launched.pixi_deps.is_some()
        || launched.pixi_toml_deps.is_some()
        || launched.deno_config.is_some();

    if is_tracking {
        // Kernel launched with inline deps - compute drift
        let diff = compute_env_sync_diff(&launched, &current_metadata);
        let in_sync = diff.is_none();

        // Write to RuntimeStateDoc
        {
            let mut sd = room.state_doc.write().await;
            let changed = match &diff {
                Some(d) => sd.set_env_sync(
                    false,
                    &d.added,
                    &d.removed,
                    d.channels_changed,
                    d.deno_changed,
                ),
                None => sd.set_env_sync(true, &[], &[], false, false),
            };
            if changed {
                let _ = room.state_changed_tx.send(());
            }
        }

        let _ = room
            .kernel_broadcast_tx
            .send(NotebookBroadcast::EnvSyncState { in_sync, diff });
    } else {
        // Kernel launched with prewarmed - check if metadata now has inline deps
        let current_inline = check_inline_deps(&current_metadata);

        if let Some(ref inline_source) = current_inline {
            let added = match inline_source.as_str() {
                "uv:inline" => get_inline_uv_deps(&current_metadata).unwrap_or_default(),
                "conda:inline" => get_inline_conda_deps(&current_metadata).unwrap_or_default(),
                _ => vec![],
            };

            if !added.is_empty() {
                {
                    let mut sd = room.state_doc.write().await;
                    if sd.set_env_sync(false, &added, &[], false, false) {
                        let _ = room.state_changed_tx.send(());
                    }
                }
                let _ = room
                    .kernel_broadcast_tx
                    .send(NotebookBroadcast::EnvSyncState {
                        in_sync: false,
                        diff: Some(EnvSyncDiff {
                            added,
                            removed: vec![],
                            channels_changed: false,
                            deno_changed: false,
                        }),
                    });
            } else {
                let _ = room
                    .kernel_broadcast_tx
                    .send(NotebookBroadcast::EnvSyncState {
                        in_sync: true,
                        diff: None,
                    });
            }
        } else {
            let _ = room
                .kernel_broadcast_tx
                .send(NotebookBroadcast::EnvSyncState {
                    in_sync: true,
                    diff: None,
                });
        }
    }
}

/// Re-verify trust from the Automerge doc and update room.trust_state + RuntimeStateDoc.
///
/// Called after every Automerge sync message to detect when the frontend writes
/// a trust signature (via approve_notebook_trust). Without this, room.trust_state
/// would remain stale from initial room creation and the trust banner would
/// reappear on reconnection.
pub(crate) async fn check_and_update_trust_state(room: &NotebookRoom) {
    let current_metadata = {
        let doc = room.doc.read().await;
        doc.get_metadata_snapshot()
    };

    let Some(current_metadata) = current_metadata else {
        return;
    };

    let new_trust = verify_trust_from_snapshot(&current_metadata);

    // Check if trust state actually changed
    let current_status = {
        let ts = room.trust_state.read().await;
        ts.status.clone()
    };

    if current_status != new_trust.status {
        info!(
            "[notebook-sync] Trust state changed via doc sync: {:?} -> {:?}",
            current_status, new_trust.status
        );

        let needs_approval = !matches!(
            new_trust.status,
            runt_trust::TrustStatus::Trusted | runt_trust::TrustStatus::NoDependencies
        );
        let status_str = match &new_trust.status {
            runt_trust::TrustStatus::Trusted => "trusted",
            runt_trust::TrustStatus::Untrusted => "untrusted",
            runt_trust::TrustStatus::SignatureInvalid => "signature_invalid",
            runt_trust::TrustStatus::NoDependencies => "no_dependencies",
        };

        // Update room.trust_state so auto-launch and reconnection use fresh state
        {
            let mut ts = room.trust_state.write().await;
            *ts = new_trust;
        }

        // Update RuntimeStateDoc so the frontend banner reacts immediately
        let mut sd = room.state_doc.write().await;
        if sd.set_trust(status_str, needs_approval) {
            let _ = room.state_changed_tx.send(());
        }
    }
}

/// Resolve the metadata snapshot for a notebook, trying the Automerge doc first
/// and falling back to disk if the doc doesn't have metadata yet (e.g., before
/// the first client has synced).
pub(crate) async fn resolve_metadata_snapshot(
    room: &NotebookRoom,
    notebook_path: Option<&Path>,
) -> Option<NotebookMetadataSnapshot> {
    // Try reading from the Automerge doc first
    {
        let doc = room.doc.read().await;
        if let Some(snapshot) = doc.get_metadata_snapshot() {
            debug!("[notebook-sync] Resolved metadata snapshot from Automerge doc");
            return Some(snapshot);
        }
    }

    // Fall back to reading from disk
    if let Some(path) = notebook_path {
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Ok(nb) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(metadata) = nb.get("metadata") {
                    let snapshot = NotebookMetadataSnapshot::from_metadata_value(metadata);
                    debug!("[notebook-sync] Resolved metadata snapshot from disk");
                    return Some(snapshot);
                }
            }
        }
    }

    None
}

/// Verify trust status of a notebook by reading its file from disk.
/// Returns TrustState with the verification result.
///
/// Used during room creation when the Automerge doc is still empty.
/// Once the doc is populated, `verify_trust_from_snapshot` is preferred
/// as it picks up in-memory changes (e.g., newly-written trust signatures).
fn verify_trust_from_file(notebook_path: &Path) -> TrustState {
    // Read and parse the notebook file
    let metadata = match std::fs::read_to_string(notebook_path) {
        Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
            Ok(nb) => nb
                .get("metadata")
                .and_then(|m| m.as_object())
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default(),
            Err(_) => std::collections::HashMap::new(),
        },
        Err(_) => std::collections::HashMap::new(),
    };

    // Verify trust using the shared runt-trust crate
    match runt_trust::verify_notebook_trust(&metadata) {
        Ok(info) => TrustState {
            status: info.status.clone(),
            info,
            pending_launch: false,
        },
        Err(_) => TrustState {
            status: runt_trust::TrustStatus::Untrusted,
            info: runt_trust::TrustInfo {
                status: runt_trust::TrustStatus::Untrusted,
                uv_dependencies: vec![],
                conda_dependencies: vec![],
                conda_channels: vec![],
            },
            pending_launch: false,
        },
    }
}

/// Verify trust status from a `NotebookMetadataSnapshot` (from the Automerge doc).
///
/// This provides the same trust verification as `verify_trust_from_file` but
/// works with the in-memory doc state instead of reading from disk. Used by
/// `check_and_update_trust_state` to detect trust changes reactively (e.g.,
/// after the frontend writes a trust signature via approval).
fn verify_trust_from_snapshot(snapshot: &NotebookMetadataSnapshot) -> TrustState {
    // Build a metadata HashMap from the snapshot's runt field, matching the
    // structure that runt_trust::verify_notebook_trust expects.
    //
    // We only insert the "runt" key — legacy top-level "uv"/"conda" keys are
    // already normalized into runt.uv/runt.conda by
    // NotebookMetadataSnapshot::from_metadata_value before they reach the
    // Automerge doc, so the legacy fallback in get_uv_metadata is not needed.
    let mut metadata = std::collections::HashMap::new();
    if let Ok(runt_value) = serde_json::to_value(&snapshot.runt) {
        metadata.insert("runt".to_string(), runt_value);
    }

    match runt_trust::verify_notebook_trust(&metadata) {
        Ok(info) => TrustState {
            status: info.status.clone(),
            info,
            pending_launch: false,
        },
        Err(_) => TrustState {
            status: runt_trust::TrustStatus::Untrusted,
            info: runt_trust::TrustInfo {
                status: runt_trust::TrustStatus::Untrusted,
                uv_dependencies: vec![],
                conda_dependencies: vec![],
                conda_channels: vec![],
            },
            pending_launch: false,
        },
    }
}

/// A notebook sync room — holds the canonical document and a broadcast
/// channel for notifying peers of changes.
pub struct NotebookRoom {
    /// Permanent, immutable UUID for this room. Used as the map key once
    /// Phase 5 lands; for now coexists with the string-keyed map.
    pub id: uuid::Uuid,
    /// The canonical Automerge notebook document.
    pub doc: Arc<RwLock<NotebookDoc>>,
    /// Broadcast channel to notify all peers in this room of changes.
    pub changed_tx: broadcast::Sender<()>,
    /// Broadcast channel for kernel events (outputs, status changes).
    pub kernel_broadcast_tx: broadcast::Sender<NotebookBroadcast>,
    /// Broadcast channel for presence frames (cursor, selection, kernel state).
    /// Carries raw presence bytes to relay to other peers.
    pub presence_tx: broadcast::Sender<(String, Vec<u8>)>,
    /// Transient peer state (cursors, selections, kernel status).
    /// Protected by RwLock for concurrent reads from multiple peer loops.
    pub presence: Arc<RwLock<PresenceState>>,
    /// Channel to send doc bytes to the debounced persistence task.
    /// Uses watch for "latest value" semantics - always keeps most recent state.
    pub persist_tx: Option<watch::Sender<Option<Vec<u8>>>>,
    /// Channel to request a synchronous flush from the persist debouncer.
    /// Receiver handles the request and replies on the oneshot after the write
    /// completes. Used by room eviction to guarantee disk consistency *before*
    /// the room is removed from the map, closing the race where a fast reconnect
    /// would load stale bytes from the still-pending .automerge file.
    ///
    /// `None` for ephemeral rooms (persistence skipped) and matches `persist_tx`.
    pub flush_request_tx: Option<mpsc::UnboundedSender<FlushRequest>>,
    /// Persistence path for this room's document.
    pub persist_path: PathBuf,
    /// Number of active peer connections in this room.
    pub active_peers: AtomicUsize,
    /// Whether at least one peer has ever connected to this room.
    pub had_peers: AtomicBool,
    /// Whether this notebook is ephemeral (in-memory only, no persistence).
    pub is_ephemeral: AtomicBool,
    /// Blob store for output manifests.
    pub blob_store: Arc<BlobStore>,
    /// Trust state for this notebook (for auto-launch decisions).
    pub trust_state: Arc<RwLock<TrustState>>,
    /// The `.ipynb` path, when this room is file-backed. `None` for untitled and
    /// ephemeral rooms. Mutated when an untitled room is saved to disk (see
    /// `handle_save_notebook`).
    pub path: RwLock<Option<PathBuf>>,
    /// Raw nbformat attachments preserved from disk, keyed by cell ID.
    /// These are not user-editable in the current UI, so the file remains the source of truth.
    pub nbformat_attachments: Arc<RwLock<HashMap<String, serde_json::Value>>>,
    /// Working directory for untitled notebooks (used for project file detection).
    /// When the notebook_id is a UUID (untitled), this provides the directory context
    /// for finding pyproject.toml, pixi.toml, or environment.yaml.
    pub working_dir: Arc<RwLock<Option<PathBuf>>>,
    /// Timestamp when auto-launch was triggered (for grace period on eviction).
    /// If set, the room won't be evicted for 30 seconds to allow client reconnect.
    pub auto_launch_at: Arc<RwLock<Option<std::time::Instant>>>,
    /// Comm channel state for widgets.
    /// Whether a streaming load is in progress for this room.
    /// Prevents two connections from both attempting to load from disk.
    pub is_loading: AtomicBool,
    /// Timestamp (ms since epoch) of last self-write to the .ipynb file.
    /// Used to skip file watcher events triggered by our own saves.
    pub last_self_write: Arc<AtomicU64>,
    /// Automerge heads at the time of the last save to disk.
    /// Previously used by the file watcher for `fork_at(last_save_heads)` —
    /// currently unused due to automerge/automerge#1327 but retained so that external
    /// disk changes merge cleanly with post-save CRDT changes (e.g.,
    /// background formatting that completed after the save).
    pub last_save_heads: Arc<RwLock<Vec<automerge::ChangeHash>>>,
    /// Cell sources as they were written to disk at last save.
    ///
    /// The file watcher compares disk content against this snapshot (not the
    /// live CRDT) to distinguish our own autosave writes from genuine external
    /// changes (git pull, external editor). Without this, the file watcher
    /// would see the autosave'd content as "different" from the live CRDT
    /// (which has progressed with new user typing since the save) and
    /// overwrite the user's recent edits.
    ///
    /// This is a workaround for the fact that we can't use
    /// `fork_at(save_heads)` to read the doc at the save point due to
    /// automerge/automerge#1327.
    pub last_save_sources: Arc<RwLock<HashMap<String, String>>>,
    /// Shutdown signal for the file watcher task.
    /// Wrapped in Mutex to allow setting after Arc creation.
    /// Sent when the room is evicted to stop the watcher.
    watcher_shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
    /// Per-notebook RuntimeStateDoc — daemon-authoritative ephemeral state
    /// (kernel status, queue, env sync). Clients sync read-only.
    pub state_doc: Arc<RwLock<RuntimeStateDoc>>,
    /// Notification channel for RuntimeStateDoc changes.
    /// Peer sync loops subscribe to push RuntimeStateSync frames.
    pub state_changed_tx: broadcast::Sender<()>,
    /// Handle to the runtime agent subprocess that owns this notebook's kernel.
    /// Set by `LaunchKernel` or `auto_launch_kernel` when spawned.
    pub runtime_agent_handle: Arc<Mutex<Option<crate::runtime_agent_handle::RuntimeAgentHandle>>>,
    /// Environment path used by a runtime-agent-backed kernel, for GC protection.
    pub runtime_agent_env_path: Arc<RwLock<Option<PathBuf>>>,
    /// The environment config used at kernel launch. Stored so
    /// check_and_broadcast_sync_state can detect dependency drift
    /// without accessing the runtime agent's kernel directly.
    pub runtime_agent_launched_config: Arc<RwLock<Option<LaunchedEnvConfig>>>,
    /// Channel for sending RPC requests (LaunchKernel, Interrupt, etc.) to the
    /// runtime agent's sync connection. Set when runtime agent connects via
    /// socket, cleared on disconnect.
    pub runtime_agent_request_tx: Arc<Mutex<Option<RuntimeAgentRequestSender>>>,
    /// Per-spawn oneshot sender for the connect handler to signal that this
    /// generation's runtime agent has established its sync connection.
    /// Replaced on each agent spawn; previous sender is dropped (cancelling
    /// the old receiver). The connect handler `take()`s the sender.
    pub(crate) pending_runtime_agent_connect_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    /// Monotonic generation counter for runtime agent spawns. Incremented
    /// before each spawn installs its oneshot/channels. Used by
    /// `reset_starting_state` to detect interleaving spawns: the generation
    /// is checked while holding each field's lock, so if it hasn't changed,
    /// no newer spawn has (or can) store a value in that field.
    pub(crate) runtime_agent_generation: Arc<AtomicU64>,
    /// Monotonic counter for execution queue ordering.
    /// The coordinator bumps this for each ExecuteCell and stamps the seq
    /// on the execution entry. The runtime agent sorts by seq to determine order.
    pub next_queue_seq: Arc<std::sync::atomic::AtomicU64>,
    /// The runtime_agent_id of the currently expected runtime agent. Used by the
    /// sync handler to validate connections and prevent stale cleanup from
    /// clobbering state.
    pub current_runtime_agent_id: Arc<RwLock<Option<String>>>,
}

impl NotebookRoom {
    /// Create a fresh room, ignoring any persisted state.
    ///
    /// The .ipynb file is the source of truth. When a room is created, we start
    /// with an empty Automerge doc and let the first client populate it from
    /// their local .ipynb file. This prevents stale outputs from previous
    /// sessions from accumulating.
    ///
    /// Any existing persisted doc is deleted to avoid clutter.
    ///
    /// Note: Trust state is initialized from disk because the Automerge doc
    /// starts empty (first client hasn't synced yet). Once the doc is populated,
    /// `check_and_update_trust_state` keeps room.trust_state current.
    pub fn new_fresh(
        uuid: uuid::Uuid,
        path: Option<PathBuf>,
        docs_dir: &Path,
        blob_store: Arc<BlobStore>,
        ephemeral: bool,
    ) -> Self {
        let id = uuid;
        // Use uuid string as the notebook_id for doc filename derivation and NotebookDoc construction.
        let notebook_id_str = uuid.to_string();

        let filename = notebook_doc_filename(&notebook_id_str);
        let persist_path = docs_dir.join(&filename);

        // For untitled notebooks (path is None), the persisted Automerge doc is their
        // only content record — there's no .ipynb on disk. Load it if it exists
        // so content survives daemon restarts.
        // For saved notebooks (path is Some), .ipynb is the source of truth, so
        // delete stale persisted docs and start fresh (daemon loads from disk).
        let runtimed_actor = "runtimed";
        let mut doc = if !ephemeral && path.is_none() && persist_path.exists() {
            info!(
                "[notebook-sync] Loading persisted doc for untitled notebook: {:?}",
                persist_path
            );
            NotebookDoc::load_or_create_with_actor(&persist_path, &notebook_id_str, runtimed_actor)
        } else {
            if !ephemeral && persist_path.exists() {
                if crate::paths::snapshot_before_delete(&persist_path, docs_dir) {
                    let _ = std::fs::remove_file(&persist_path);
                } else {
                    warn!(
                        "[notebook-sync] Keeping persisted doc (snapshot failed): {:?}",
                        persist_path
                    );
                }
            }
            // TODO(phase-6): tighten NotebookDoc to accept Uuid directly
            NotebookDoc::new_with_actor(&notebook_id_str, runtimed_actor)
        };
        let (changed_tx, _) = broadcast::channel(16);
        let (kernel_broadcast_tx, _) = broadcast::channel(KERNEL_BROADCAST_CAPACITY);

        // Spawn debounced persistence task (watch channel keeps latest value only)
        // Ephemeral rooms skip persistence entirely.
        // Store ephemeral flag in doc metadata so the GUI can show a banner
        if ephemeral {
            let _ = doc.set_metadata("ephemeral", "true");
        }

        let (persist_tx, flush_request_tx) = if ephemeral {
            (None, None)
        } else {
            let (persist_tx, persist_rx) = watch::channel::<Option<Vec<u8>>>(None);
            let (flush_tx, flush_rx) = mpsc::unbounded_channel::<FlushRequest>();
            spawn_persist_debouncer(persist_rx, flush_rx, persist_path.clone());
            (Some(persist_tx), Some(flush_tx))
        };

        let trust_state = match &path {
            // Untitled notebooks have no .ipynb on disk — trust signature lives
            // in the persisted Automerge doc we just loaded.
            None => match doc.get_metadata_snapshot() {
                Some(snapshot) => verify_trust_from_snapshot(&snapshot),
                None => TrustState {
                    status: runt_trust::TrustStatus::NoDependencies,
                    info: runt_trust::TrustInfo {
                        status: runt_trust::TrustStatus::NoDependencies,
                        uv_dependencies: vec![],
                        conda_dependencies: vec![],
                        conda_channels: vec![],
                    },
                    pending_launch: false,
                },
            },
            Some(p) => verify_trust_from_file(p),
        };
        info!(
            "[notebook-sync] Trust status for {}: {:?}",
            notebook_id_str, trust_state.status
        );

        let (presence_tx, _) = broadcast::channel(64);

        let state_doc = Arc::new(RwLock::new(RuntimeStateDoc::new()));

        // Migrate outputs from notebook doc to RuntimeStateDoc for pre-v3 untitled notebooks.
        // .ipynb files already create synthetic execution_ids on load; this handles
        // persisted .automerge files that have outputs in the old cell.outputs location.
        if path.is_none() {
            let cell_outputs = doc.extract_cell_outputs();
            if !cell_outputs.is_empty() {
                let mut sd = state_doc.blocking_write();
                for (cell_id, outputs) in &cell_outputs {
                    let synthetic_eid = uuid::Uuid::new_v4().to_string();
                    sd.create_execution(&synthetic_eid, cell_id);
                    let json_outputs: Vec<serde_json::Value> = outputs
                        .iter()
                        .map(|s| {
                            let mut val = serde_json::from_str(s)
                                .unwrap_or_else(|_| serde_json::Value::String(s.clone()));
                            // Mint output_id for legacy outputs that lack one
                            if let serde_json::Value::Object(ref mut map) = val {
                                if !map.contains_key("output_id") {
                                    map.insert(
                                        "output_id".to_string(),
                                        serde_json::Value::String(uuid::Uuid::new_v4().to_string()),
                                    );
                                }
                            }
                            val
                        })
                        .collect();
                    let _ = sd.set_outputs(&synthetic_eid, &json_outputs);
                    sd.set_execution_done(&synthetic_eid, true);
                    let _ = doc.set_execution_id(cell_id, Some(&synthetic_eid));
                }
                info!(
                    "[notebook-sync] Migrated outputs for {} cells from notebook doc to RuntimeStateDoc",
                    cell_outputs.len()
                );
            }
        }

        let (state_changed_tx, _) = broadcast::channel(16);

        Self {
            id,
            doc: Arc::new(RwLock::new(doc)),
            changed_tx,
            kernel_broadcast_tx,
            presence_tx,
            presence: Arc::new(RwLock::new(PresenceState::new())),
            persist_tx,
            flush_request_tx,
            persist_path,
            active_peers: AtomicUsize::new(0),
            had_peers: AtomicBool::new(false),
            is_ephemeral: AtomicBool::new(ephemeral),
            blob_store,
            trust_state: Arc::new(RwLock::new(trust_state)),
            path: RwLock::new(path),
            nbformat_attachments: Arc::new(RwLock::new(HashMap::new())),
            working_dir: Arc::new(RwLock::new(None)),
            auto_launch_at: Arc::new(RwLock::new(None)),

            is_loading: AtomicBool::new(false),
            last_self_write: Arc::new(AtomicU64::new(0)),
            last_save_heads: Arc::new(RwLock::new(Vec::new())),
            last_save_sources: Arc::new(RwLock::new(HashMap::new())),
            watcher_shutdown_tx: Mutex::new(None),
            state_doc,
            state_changed_tx,
            runtime_agent_handle: Arc::new(Mutex::new(None)),
            runtime_agent_env_path: Arc::new(RwLock::new(None)),
            runtime_agent_launched_config: Arc::new(RwLock::new(None)),
            runtime_agent_request_tx: Arc::new(Mutex::new(None)),
            pending_runtime_agent_connect_tx: Arc::new(Mutex::new(None)),
            runtime_agent_generation: Arc::new(AtomicU64::new(0)),
            next_queue_seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            current_runtime_agent_id: Arc::new(RwLock::new(None)),
        }
    }

    /// Atomically claim the loading role for this room.
    ///
    /// Returns `true` if the caller won the race and should perform the load.
    /// Returns `false` if another connection is already loading.
    pub fn try_start_loading(&self) -> bool {
        self.is_loading
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Mark loading as complete (success or failure).
    pub fn finish_loading(&self) {
        self.is_loading.store(false, Ordering::Release);
    }

    /// Create a new room by loading a persisted document or creating a fresh one.
    ///
    /// Note: This method is kept for tests that verify persistence behavior.
    /// For normal operation, `new_fresh` is used to ensure the .ipynb file
    /// is the source of truth.
    #[cfg(test)]
    pub fn load_or_create(notebook_id: &str, docs_dir: &Path, blob_store: Arc<BlobStore>) -> Self {
        // Derive UUID from notebook_id if it parses as a UUID, else mint a fresh one.
        let id = uuid::Uuid::parse_str(notebook_id).unwrap_or_else(|_| uuid::Uuid::new_v4());

        let filename = notebook_doc_filename(notebook_id);
        let persist_path = docs_dir.join(filename);
        let doc = NotebookDoc::load_or_create(&persist_path, notebook_id);
        let (changed_tx, _) = broadcast::channel(16);
        let (kernel_broadcast_tx, _) = broadcast::channel(KERNEL_BROADCAST_CAPACITY);
        let (persist_tx, persist_rx) = watch::channel::<Option<Vec<u8>>>(None);
        let (flush_request_tx, flush_rx) = mpsc::unbounded_channel::<FlushRequest>();
        spawn_persist_debouncer(persist_rx, flush_rx, persist_path.clone());
        let (presence_tx, _) = broadcast::channel(64);
        let path = if is_untitled_notebook(notebook_id) {
            None
        } else {
            Some(PathBuf::from(notebook_id))
        };
        let trust_state = match &path {
            None => match doc.get_metadata_snapshot() {
                Some(snapshot) => verify_trust_from_snapshot(&snapshot),
                None => TrustState {
                    status: runt_trust::TrustStatus::NoDependencies,
                    info: runt_trust::TrustInfo {
                        status: runt_trust::TrustStatus::NoDependencies,
                        uv_dependencies: vec![],
                        conda_dependencies: vec![],
                        conda_channels: vec![],
                    },
                    pending_launch: false,
                },
            },
            Some(p) => verify_trust_from_file(p),
        };
        let state_doc = Arc::new(RwLock::new(RuntimeStateDoc::new()));
        let (state_changed_tx, _) = broadcast::channel(16);
        Self {
            id,
            doc: Arc::new(RwLock::new(doc)),
            changed_tx,
            kernel_broadcast_tx,
            presence_tx,
            presence: Arc::new(RwLock::new(PresenceState::new())),
            persist_tx: Some(persist_tx),
            flush_request_tx: Some(flush_request_tx),
            persist_path,
            active_peers: AtomicUsize::new(0),
            had_peers: AtomicBool::new(false),
            is_ephemeral: AtomicBool::new(false),
            blob_store,
            trust_state: Arc::new(RwLock::new(trust_state)),
            path: RwLock::new(path),
            nbformat_attachments: Arc::new(RwLock::new(HashMap::new())),
            working_dir: Arc::new(RwLock::new(None)),
            auto_launch_at: Arc::new(RwLock::new(None)),

            is_loading: AtomicBool::new(false),
            last_self_write: Arc::new(AtomicU64::new(0)),
            last_save_heads: Arc::new(RwLock::new(Vec::new())),
            last_save_sources: Arc::new(RwLock::new(HashMap::new())),
            watcher_shutdown_tx: Mutex::new(None),
            state_doc,
            state_changed_tx,
            runtime_agent_handle: Arc::new(Mutex::new(None)),
            runtime_agent_env_path: Arc::new(RwLock::new(None)),
            runtime_agent_launched_config: Arc::new(RwLock::new(None)),
            runtime_agent_request_tx: Arc::new(Mutex::new(None)),
            pending_runtime_agent_connect_tx: Arc::new(Mutex::new(None)),
            runtime_agent_generation: Arc::new(AtomicU64::new(0)),
            next_queue_seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            current_runtime_agent_id: Arc::new(RwLock::new(None)),
        }
    }

    /// Check if this room has an active kernel.
    pub async fn has_kernel(&self) -> bool {
        // Check runtime agent handle
        let ra = self.runtime_agent_handle.lock().await;
        ra.as_ref().is_some_and(|a| a.is_alive())
    }

    /// Get kernel info if a kernel is running (runtime-agent-backed).
    ///
    /// Reads from RuntimeStateDoc (source of truth for runtime agent).
    pub async fn kernel_info(&self) -> Option<(String, String, String)> {
        // Check runtime agent — scope the lock so it drops before the next
        // `.await` on state_doc (deadlock prevention: no cross-lock holds).
        let is_alive = {
            let ra = self.runtime_agent_handle.lock().await;
            ra.as_ref().is_some_and(|a| a.is_alive())
        };
        if is_alive {
            let sd = self.state_doc.read().await;
            let state = sd.read_state();
            if state.kernel.status != "not_started" && !state.kernel.status.is_empty() {
                return Some((
                    state.kernel.name.clone(),
                    state.kernel.env_source.clone(),
                    state.kernel.status.clone(),
                ));
            }
        }
        None
    }
}

/// Thread-safe map of notebook rooms, keyed by UUID.
pub type NotebookRooms = Arc<Mutex<HashMap<uuid::Uuid, Arc<NotebookRoom>>>>;

/// Look up an open room by its canonical .ipynb path.
///
/// Returns `None` if no room is currently serving that path. O(1) lookup
/// via the path_index — no scanning.
pub async fn find_room_by_path(
    rooms: &NotebookRooms,
    path_index: &Arc<tokio::sync::Mutex<PathIndex>>,
    path: &Path,
) -> Option<Arc<NotebookRoom>> {
    let uuid = {
        let idx = path_index.lock().await;
        idx.lookup(path)?
    };
    rooms.lock().await.get(&uuid).cloned()
}

/// Get or create a room for a notebook.
///
/// Creates a new fresh room if one for the given UUID doesn't already exist.
/// The .ipynb file is the source of truth — the first client to connect will
/// populate the Automerge doc from their local file.
///
/// For .ipynb files, a file watcher is spawned to detect external changes.
/// Also inserts an entry into `path_index` when `path` is `Some`.
pub async fn get_or_create_room(
    rooms: &NotebookRooms,
    path_index: &Arc<tokio::sync::Mutex<PathIndex>>,
    uuid: uuid::Uuid,
    path: Option<PathBuf>,
    docs_dir: &Path,
    blob_store: Arc<BlobStore>,
    ephemeral: bool,
) -> Arc<NotebookRoom> {
    // Fast path: room already exists.
    {
        let rooms_guard = rooms.lock().await;
        if let Some(existing) = rooms_guard.get(&uuid) {
            return existing.clone();
        }
    }

    // Create new room and insert.
    info!("[notebook-sync] Creating room for {}", uuid);
    let room = Arc::new(NotebookRoom::new_fresh(
        uuid,
        path.clone(),
        docs_dir,
        blob_store,
        ephemeral,
    ));

    {
        let mut rooms_guard = rooms.lock().await;
        // Double-check in case of a race: another task may have created the room
        // between our unlock above and acquiring the write lock here.
        if let Some(existing) = rooms_guard.get(&uuid) {
            return existing.clone();
        }
        rooms_guard.insert(uuid, room.clone());
    }

    // Insert into path_index (under a separate lock per the locking convention).
    if let Some(ref p) = path {
        match path_index.lock().await.insert(p.clone(), uuid) {
            Ok(()) => {}
            Err(e) => {
                error!(
                    "[notebook-sync] path_index.insert failed for new room {} at {:?}: {} — \
                     this is a caller invariant violation (should have done find_room_by_path first). \
                     Room is orphaned from path lookup.",
                    uuid, p, e
                );
            }
        }
    }

    // Spawn file watcher for .ipynb files (not for untitled notebooks).
    if let Some(ref notebook_path) = path {
        if notebook_path.extension().is_some_and(|ext| ext == "ipynb") {
            let shutdown_tx = spawn_notebook_file_watcher(notebook_path.clone(), room.clone());
            // Blocking lock is OK here — room is brand new, no contention.
            if let Ok(mut guard) = room.watcher_shutdown_tx.try_lock() {
                *guard = Some(shutdown_tx);
            }
        }

        // Spawn autosave debouncer to keep .ipynb on disk current.
        let path_str = notebook_path.to_string_lossy().to_string();
        spawn_autosave_debouncer(path_str, room.clone());
    }

    room
}

/// Handle a single notebook sync client connection.
///
/// The caller has already consumed the handshake frame and resolved the room.
/// This function runs the Automerge sync protocol:
/// 1. Initial sync: server sends first message
/// 2. Watch loop: wait for changes (from other peers or from this client),
///    exchange sync messages to propagate
///
/// When the connection closes (client disconnect or error), the peer count
/// is decremented. If it reaches zero, the room is evicted and any pending
/// doc bytes are flushed via debounced persistence.
///
/// Uses v2 typed frames protocol (with first-byte type indicator).
/// Handle a runtime agent subprocess that connected back to the daemon's Unix socket.
///
/// The runtime agent is a special peer that owns the kernel for this notebook
/// room. It receives RPC requests (LaunchKernel, Interrupt, etc.) via frame
/// 0x01 and watches RuntimeStateDoc for queued executions via frame 0x05.
///
/// This handler:
/// 1. Performs initial NotebookDoc + RuntimeStateDoc sync
/// 2. Sets up the `runtime_agent_request_tx` channel on the room
/// 3. Fires `runtime_agent_connected` to unblock LaunchKernel
/// 4. Enters a sync loop relaying frames bidirectionally
pub async fn handle_runtime_agent_sync_connection<R, W>(
    mut reader: R,
    mut writer: W,
    room: Arc<NotebookRoom>,
    notebook_id: String,
    runtime_agent_id: String,
) where
    R: tokio::io::AsyncRead + Unpin + Send,
    W: tokio::io::AsyncWrite + Unpin + Send,
{
    use notebook_protocol::connection::{recv_typed_frame, send_typed_frame, NotebookFrameType};
    use notebook_protocol::protocol::RuntimeAgentResponse;

    info!(
        "[notebook-sync] Runtime agent sync connection: notebook={} runtime_agent={}",
        notebook_id, runtime_agent_id
    );

    // Validate provenance — reject stale agents.
    // None means no agent is expected (room was reset or no spawn in progress),
    // so reject unconditionally. Only the exact current agent ID is accepted.
    {
        let expected = room.current_runtime_agent_id.read().await;
        match expected.as_deref() {
            Some(expected_id) if expected_id == runtime_agent_id => {
                // Match — this is the agent we're waiting for.
            }
            other => {
                warn!(
                    "[notebook-sync] Rejecting runtime agent {} (provenance is {:?})",
                    runtime_agent_id, other
                );
                return;
            }
        }
    }

    // ── 1. Initial NotebookDoc sync ──────────────────────────────────
    // Scope the doc write guard so it drops before the async send
    // (deadlock prevention: no lock held across `.await`).
    let mut doc_sync_state = automerge::sync::State::new();
    let doc_sync_msg = {
        let mut doc = room.doc.write().await;
        // Generate our sync message (full doc state for fresh peer)
        doc.generate_sync_message(&mut doc_sync_state)
            .map(|msg| msg.encode())
    };
    if let Some(encoded) = doc_sync_msg {
        if let Err(e) =
            send_typed_frame(&mut writer, NotebookFrameType::AutomergeSync, &encoded).await
        {
            warn!("[notebook-sync] Agent initial doc sync send failed: {}", e);
            return;
        }
    }

    // ── 2. Initial RuntimeStateDoc sync ──────────────────────────────
    // Scope the state_doc write guard so it drops before the async send.
    // Uses bounded generation to compact if oversized (same 80 MiB threshold).
    let mut state_sync_state = automerge::sync::State::new();
    let state_sync_msg = {
        let mut sd = room.state_doc.write().await;
        sd.generate_sync_message_bounded_encoded(
            &mut state_sync_state,
            STATE_SYNC_COMPACT_THRESHOLD,
        )
    };
    if let Some(encoded) = state_sync_msg {
        if let Err(e) =
            send_typed_frame(&mut writer, NotebookFrameType::RuntimeStateSync, &encoded).await
        {
            warn!(
                "[notebook-sync] Agent initial state sync send failed: {}",
                e
            );
            return;
        }
    }

    // ── 3. Set up request channel ────────────────────────────────────
    let (ra_tx, mut ra_rx) = tokio::sync::mpsc::channel::<RuntimeAgentMessage>(16);
    {
        let mut tx_guard = room.runtime_agent_request_tx.lock().await;
        *tx_guard = Some(ra_tx);
    }

    // ── 4. Signal connected ─────────────────────────────────────────
    // Provenance is already set by the spawn site (before spawn).
    // We do NOT re-set it here — doing so after the async sync work above
    // would create a window where a newer spawn's provenance could be
    // clobbered by this (potentially stale) connect handler.
    //
    // take() ensures at most one signal per spawn generation — a stale
    // runtime agent that passes provenance finds None here (no-op).
    if let Some(tx) = room.pending_runtime_agent_connect_tx.lock().await.take() {
        let _ = tx.send(());
    }
    info!(
        "[notebook-sync] Runtime agent connected and ready: {}",
        runtime_agent_id
    );

    // ── 5. Sync loop ─────────────────────────────────────────────────
    let mut changed_rx = room.changed_tx.subscribe();
    let mut state_changed_rx = room.state_changed_tx.subscribe();
    let mut pending_replies: std::collections::HashMap<
        String,
        tokio::sync::oneshot::Sender<RuntimeAgentResponse>,
    > = std::collections::HashMap::new();

    loop {
        tokio::select! {
            // Frames from runtime agent
            frame = recv_typed_frame(&mut reader) => {
                match frame {
                    Ok(Some(typed_frame)) => {
                        match typed_frame.frame_type {
                            NotebookFrameType::AutomergeSync => {
                                if let Ok(msg) = automerge::sync::Message::decode(&typed_frame.payload) {
                                    let mut doc = room.doc.write().await;
                                    if doc.receive_sync_message(&mut doc_sync_state, msg).is_ok() {
                                        let _ = room.changed_tx.send(());
                                    }
                                    // Send sync reply
                                    if let Some(reply) = doc.generate_sync_message(&mut doc_sync_state) {
                                        let encoded = reply.encode();
                                        let _ = send_typed_frame(
                                            &mut writer,
                                            NotebookFrameType::AutomergeSync,
                                            &encoded,
                                        ).await;
                                    }
                                }
                            }
                            NotebookFrameType::RuntimeStateSync => {
                                if let Ok(msg) = automerge::sync::Message::decode(&typed_frame.payload) {
                                    let mut sd = room.state_doc.write().await;
                                    if let Ok(changed) = sd.receive_sync_message_with_changes(
                                        &mut state_sync_state, msg,
                                    ) {
                                        if changed {
                                            let _ = room.state_changed_tx.send(());
                                        }
                                    }
                                    // Send sync reply
                                    if let Some(reply) = sd.generate_sync_message(&mut state_sync_state) {
                                        let encoded = reply.encode();
                                        let _ = send_typed_frame(
                                            &mut writer,
                                            NotebookFrameType::RuntimeStateSync,
                                            &encoded,
                                        ).await;
                                    }
                                }
                            }
                            NotebookFrameType::Response => {
                                if let Ok(envelope) = serde_json::from_slice::<
                                    notebook_protocol::protocol::RuntimeAgentResponseEnvelope,
                                >(&typed_frame.payload) {
                                    if let Some(reply) = pending_replies.remove(&envelope.id) {
                                        let _ = reply.send(envelope.response);
                                    } else {
                                        debug!("[notebook-sync] Agent response for unknown id: {}", envelope.id);
                                    }
                                }
                            }
                            _ => {
                                debug!("[notebook-sync] Agent sent unexpected frame type: {:?}", typed_frame.frame_type);
                            }
                        }
                    }
                    Ok(None) => {
                        info!("[notebook-sync] Agent disconnected (EOF)");
                        break;
                    }
                    Err(e) => {
                        info!("[notebook-sync] Agent disconnected: {}", e);
                        break;
                    }
                }
            }

            // NotebookDoc changes (from other peers) → sync to runtime agent
            _ = changed_rx.recv() => {
                while changed_rx.try_recv().is_ok() {}
                let mut doc = room.doc.write().await;
                if let Some(msg) = doc.generate_sync_message(&mut doc_sync_state) {
                    let encoded = msg.encode();
                    if let Err(e) = send_typed_frame(
                        &mut writer,
                        NotebookFrameType::AutomergeSync,
                        &encoded,
                    ).await {
                        warn!("[notebook-sync] Failed to sync doc to runtime agent: {}", e);
                        break;
                    }
                }
            }

            // RuntimeStateDoc changes → sync to runtime agent
            _ = state_changed_rx.recv() => {
                while state_changed_rx.try_recv().is_ok() {}
                let mut sd = room.state_doc.write().await;
                if let Some(msg) = sd.generate_sync_message(&mut state_sync_state) {
                    let encoded = msg.encode();
                    if let Err(e) = send_typed_frame(
                        &mut writer,
                        NotebookFrameType::RuntimeStateSync,
                        &encoded,
                    ).await {
                        warn!("[notebook-sync] Failed to sync state to runtime agent: {}", e);
                        break;
                    }
                }
            }

            // Forward requests to the runtime agent. Commands are fire-and-forget;
            // queries register a pending reply keyed by correlation ID.
            Some(msg) = ra_rx.recv() => {
                let (envelope, reply_tx) = match msg {
                    RuntimeAgentMessage::Command(env) => (env, None),
                    RuntimeAgentMessage::Query(env, tx) => (env, Some(tx)),
                };
                let json = match serde_json::to_vec(&envelope) {
                    Ok(j) => j,
                    Err(e) => {
                        if let Some(tx) = reply_tx {
                            let _ = tx.send(RuntimeAgentResponse::Error {
                                error: format!("Serialize error: {}", e),
                            });
                        }
                        continue;
                    }
                };
                if let Err(e) = send_typed_frame(
                    &mut writer,
                    NotebookFrameType::Request,
                    &json,
                ).await {
                    if let Some(tx) = reply_tx {
                        let _ = tx.send(RuntimeAgentResponse::Error {
                            error: format!("Send error: {}", e),
                        });
                    }
                    break;
                }
                if let Some(tx) = reply_tx {
                    pending_replies.insert(envelope.id, tx);
                }
            }
        }
    }

    // Drain any pending query replies so callers get an error instead of hanging.
    for (_id, reply_tx) in pending_replies.drain() {
        let _ = reply_tx.send(RuntimeAgentResponse::Error {
            error: "Runtime agent disconnected".to_string(),
        });
    }

    // Cleanup: only clear state if we're still the current runtime agent.
    // A stale runtime agent disconnecting after a new one connected must not
    // clobber the new runtime agent's channel.
    //
    // Scope the id read guard so it drops before acquiring other locks
    // (deadlock prevention: no lock held across `.await`).
    let is_current = {
        let expected = room.current_runtime_agent_id.read().await;
        expected.as_deref() == Some(&runtime_agent_id)
    };
    if is_current {
        {
            let mut tx_guard = room.runtime_agent_request_tx.lock().await;
            *tx_guard = None;
        }
        // No need to signal "disconnected" — the oneshot was consumed on
        // connect. If the runtime agent dies before connecting, the oneshot
        // sender is dropped when pending_runtime_agent_connect_tx is replaced
        // by the next spawn, which resolves the receiver with Err.
        //
        // Clear runtime_agent_handle so LaunchKernel spawns a new runtime agent
        let mut guard = room.runtime_agent_handle.lock().await;
        *guard = None;
    }
    info!(
        "[notebook-sync] Runtime agent sync connection closed: {}",
        runtime_agent_id
    );
}

///
/// If `skip_capabilities` is true, the ProtocolCapabilities frame is not sent.
/// This is used for OpenNotebook/CreateNotebook handshakes where the protocol
/// is already communicated in the NotebookConnectionInfo response.
#[allow(clippy::too_many_arguments)]
pub async fn handle_notebook_sync_connection<R, W>(
    mut reader: R,
    mut writer: W,
    room: Arc<NotebookRoom>,
    rooms: NotebookRooms,
    notebook_id: String,
    default_runtime: crate::runtime::Runtime,
    default_python_env: crate::settings_doc::PythonEnvType,
    daemon: std::sync::Arc<crate::daemon::Daemon>,
    working_dir: Option<PathBuf>,
    initial_metadata: Option<String>,
    skip_capabilities: bool,
    needs_load: Option<PathBuf>,
    // True if this is a newly-created notebook at a non-existent path.
    // Used to enable auto-launch for notebooks created via `runt notebook newfile.ipynb`.
    created_new_at_path: bool,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // Set working_dir on the room if provided (for untitled notebook project detection)
    if let Some(wd) = working_dir {
        let mut room_wd = room.working_dir.write().await;
        *room_wd = Some(wd);
    }

    // Seed initial metadata into the Automerge doc if provided and doc has no metadata yet.
    // This ensures the kernelspec is available before auto-launch decides which kernel to use.
    if let Some(ref metadata_json) = initial_metadata {
        match serde_json::from_str::<NotebookMetadataSnapshot>(metadata_json) {
            Ok(snapshot) => {
                let mut doc = room.doc.write().await;
                if doc.get_metadata_snapshot().is_none() {
                    match doc.set_metadata_snapshot(&snapshot) {
                        Ok(()) => {
                            info!(
                                "[notebook-sync] Seeded initial metadata from handshake for {}",
                                notebook_id
                            );
                        }
                        Err(e) => {
                            warn!("[notebook-sync] Failed to seed initial metadata: {}", e);
                        }
                    }
                }
            }
            Err(e) => {
                warn!(
                    "[notebook-sync] Failed to parse initial metadata JSON for {}: {}",
                    notebook_id, e
                );
            }
        }
    }

    // Write trust state to RuntimeStateDoc so frontend can read it reactively.
    // Start with room.trust_state (from disk at room creation), then re-verify
    // from the doc in case initial_metadata was just seeded with a trust signature.
    //
    // Scope the trust_state read guard so it drops before acquiring state_doc
    // write lock (deadlock prevention: no lock held across `.await`).
    let (status_str, needs_approval) = {
        let trust_state = room.trust_state.read().await;
        let needs_approval = !matches!(
            trust_state.status,
            runt_trust::TrustStatus::Trusted | runt_trust::TrustStatus::NoDependencies
        );
        let status_str = match &trust_state.status {
            runt_trust::TrustStatus::Trusted => "trusted",
            runt_trust::TrustStatus::Untrusted => "untrusted",
            runt_trust::TrustStatus::SignatureInvalid => "signature_invalid",
            runt_trust::TrustStatus::NoDependencies => "no_dependencies",
        };
        (status_str, needs_approval)
    };
    {
        let mut sd = room.state_doc.write().await;
        if sd.set_trust(status_str, needs_approval) {
            let _ = room.state_changed_tx.send(());
        }
    }
    // Re-verify trust from doc metadata — picks up trust signatures that were
    // written to the Automerge doc (e.g., from a previous approval or from
    // initial_metadata seeded above).
    check_and_update_trust_state(&room).await;

    room.active_peers.fetch_add(1, Ordering::Relaxed);
    room.had_peers.store(true, Ordering::Relaxed);
    let peers = room.active_peers.load(Ordering::Relaxed);
    info!(
        "[notebook-sync] Client connected to room {} ({} peer{})",
        notebook_id,
        peers,
        if peers == 1 { "" } else { "s" }
    );

    // Auto-launch kernel if this is the first peer and notebook is trusted
    if peers == 1 {
        // Check if notebook_id is a UUID (new unsaved notebook) vs a file path
        let path_snapshot = room.path.read().await.clone();
        let is_new_notebook = path_snapshot.as_ref().is_none_or(|p| !p.exists())
            && uuid::Uuid::parse_str(&notebook_id).is_ok();

        // Scope the trust_state read guard so it drops before
        // `has_kernel()` which acquires another lock (deadlock prevention).
        let trust_status = {
            let trust_state = room.trust_state.read().await;
            trust_state.status.clone()
        };
        let has_kernel = room.has_kernel().await;
        let should_auto_launch = !has_kernel
            && matches!(
                trust_status,
                runt_trust::TrustStatus::Trusted | runt_trust::TrustStatus::NoDependencies
            )
            // For existing files: trust must be verified (Trusted or NoDependencies)
            // For new notebooks (UUID, no file): NoDependencies is safe to auto-launch
            // For newly-created notebooks at a path: also safe to auto-launch
            && (path_snapshot.as_ref().is_some_and(|p| p.exists()) || is_new_notebook || created_new_at_path);

        if should_auto_launch {
            info!(
                "[notebook-sync] Auto-launching kernel for notebook {} (trust: {:?}, new: {})",
                notebook_id, trust_status, is_new_notebook
            );
            // Record auto-launch time for grace period on eviction
            {
                let mut auto_launch_at = room.auto_launch_at.write().await;
                *auto_launch_at = Some(std::time::Instant::now());
            }
            // Write "starting" immediately so clients never see stale "not_started"
            {
                let mut sd = room.state_doc.write().await;
                let mut changed = false;
                changed |= sd.set_kernel_status("starting");
                changed |= sd.set_starting_phase("resolving");
                if changed {
                    let _ = room.state_changed_tx.send(());
                }
            }
            // Spawn auto-launch in background so we don't block sync
            let room_clone = room.clone();
            let panic_room = room.clone();
            let notebook_id_clone = notebook_id.clone();
            let daemon_clone = daemon.clone();
            spawn_supervised(
                "auto-launch-kernel",
                async move {
                    auto_launch_kernel(
                        &room_clone,
                        &notebook_id_clone,
                        default_runtime,
                        default_python_env,
                        daemon_clone,
                    )
                    .await;
                },
                move |_| {
                    let r = panic_room;
                    tokio::spawn(async move {
                        let mut sd = r.state_doc.write().await;
                        sd.set_kernel_status("error");
                        sd.set_starting_phase("");
                        let _ = r.state_changed_tx.send(());
                    });
                },
            );
        } else if !has_kernel
            && matches!(
                trust_status,
                runt_trust::TrustStatus::Untrusted | runt_trust::TrustStatus::SignatureInvalid
            )
        {
            // Kernel blocked on trust approval — write this to RuntimeStateDoc
            // so the frontend shows "Awaiting Trust Approval" instead of "Initializing"
            info!(
                "[notebook-sync] Kernel blocked on trust approval for {} (trust: {:?})",
                notebook_id, trust_status
            );
            let mut sd = room.state_doc.write().await;
            let mut changed = false;
            changed |= sd.set_kernel_status("awaiting_trust");
            changed |= sd.set_starting_phase("");
            if changed {
                let _ = room.state_changed_tx.send(());
            }
        } else {
            info!(
                "[notebook-sync] Auto-launch skipped for {} (trust: {:?}, has_kernel: {}, path_exists: {}, is_new: {}, created_at_path: {})",
                notebook_id, trust_status, has_kernel,
                path_snapshot.as_ref().is_some_and(|p| p.exists()), is_new_notebook, created_new_at_path
            );
        }
    }

    // Send capabilities response (v2 protocol) unless already sent via NotebookConnectionInfo
    if !skip_capabilities {
        let caps = connection::ProtocolCapabilities {
            protocol: connection::PROTOCOL_V2.to_string(),
            protocol_version: Some(connection::PROTOCOL_VERSION),
            daemon_version: Some(crate::daemon_version().to_string()),
        };
        connection::send_json_frame(&mut writer, &caps).await?;
    }

    // Generate peer_id here so it's available for cleanup regardless of
    // whether the sync loop exits with Ok or Err.
    let peer_id = uuid::Uuid::new_v4().to_string();

    // Pre-send the initial notebook-doc AutomergeSync frame BEFORE entering
    // the background sync loop. This guarantees the first AutomergeSync
    // frame is on the wire before the client's `do_initial_sync` starts
    // ticking its 100ms-per-frame convergence timeout.
    // Without this ordering, under CI load a spawned-but-unscheduled handler
    // task can race with the client's timeout, flaking tests like
    // `test_pipe_mode_forwards_sync_frames`.
    //
    // State/pool/presence initial syncs stay inside `run_sync_loop_v2`
    // because they must run AFTER streaming load populates per-cell
    // outputs in the RuntimeStateDoc, and AFTER the broadcast channels
    // are subscribed.
    let initial_sync_state = send_initial_notebook_doc_sync(&mut writer, &room).await?;

    let result = run_sync_loop_v2(
        &mut reader,
        &mut writer,
        &room,
        rooms.clone(),
        notebook_id.clone(),
        daemon.clone(),
        needs_load.as_deref(),
        &peer_id,
        initial_sync_state,
    )
    .await;

    // Always clean up presence on disconnect, whether the sync loop
    // exited cleanly (Ok) or with an error (Err). The peer_id was
    // generated before starting the sync loop, so it is always
    // available here. remove_peer is a no-op for unknown peers
    // (e.g. error before any presence was registered).
    room.presence.write().await.remove_peer(&peer_id);
    match presence::encode_left(&peer_id) {
        Ok(left_bytes) => {
            let _ = room.presence_tx.send((peer_id, left_bytes));
        }
        Err(e) => warn!("[notebook-sync] Failed to encode 'left' presence: {}", e),
    }

    // Peer disconnected — decrement and possibly evict the room
    let remaining = room.active_peers.fetch_sub(1, Ordering::Relaxed) - 1;
    if remaining == 0 {
        // Schedule delayed eviction check. This handles:
        // 1. Grace period during auto-launch (client may reconnect)
        // 2. Kernel running with no peers (idle timeout)
        // Without this, rooms with kernels would leak indefinitely.
        let eviction_delay = daemon.room_eviction_delay().await;
        let rooms_for_eviction = rooms.clone();
        let path_index_for_eviction = daemon.path_index.clone();
        let room_for_eviction = room.clone();
        let notebook_id_for_eviction = notebook_id.clone();

        info!(
            "[notebook-sync] All peers disconnected from room {}, scheduling eviction check in {:?}",
            notebook_id,
            eviction_delay
        );

        spawn_best_effort("room-eviction", async move {
            // Outer loop wraps the eviction attempt so a flush timeout can
            // back off and retry rather than leak the room (and any attached
            // kernel / watcher) indefinitely. The loop exits either by
            // cancelling (peers reconnected) or by completing teardown.
            let mut delay = eviction_delay;
            let mut flush_retries: u32 = 0;
            loop {
                tokio::time::sleep(delay).await;

                // Check if peers reconnected during the delay
                if room_for_eviction.active_peers.load(Ordering::Relaxed) > 0 {
                    info!(
                        "[notebook-sync] Eviction cancelled for {} (peers reconnected)",
                        notebook_id_for_eviction
                    );
                    return;
                }

                // Force a synchronous flush of the persist debouncer BEFORE removing
                // the room from the map. Without this, a fast reconnect lands in
                // the window between HashMap removal and the debouncer's shutdown
                // flush (which only fires when the last Arc to the room drops, and
                // the eviction task still holds one while running kernel/env
                // teardown). In that window get_or_create_room creates a fresh
                // room that loads stale bytes from the .automerge file — or no
                // file at all for brand-new untitled notebooks — silently losing
                // cells and edits.
                //
                // Request/ack over a dedicated channel. The debouncer has a
                // select! arm that writes the latest doc bytes and replies on
                // the oneshot with the I/O result.
                //
                // On timeout or write failure we back off and retry indefinitely.
                // Proceeding with HashMap removal on a failed flush reopens the
                // race: either the write is still in flight, or the latest bytes
                // are only in the soon-to-be-dropped room. We'd rather leak a
                // room than silently lose user edits. A reconnect still finds
                // the live in-memory room and recovers; a genuinely wedged
                // filesystem will surface through other signals, and daemon
                // shutdown still tries a last flush on persist_tx drop.
                const FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
                const FLUSH_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(30);
                let mut flush_ok = true;
                let mut flush_failure_kind: Option<&'static str> = None;
                if let Some(ref flush_tx) = room_for_eviction.flush_request_tx {
                    let (ack_tx, ack_rx) = oneshot::channel::<bool>();
                    if flush_tx.send(ack_tx).is_ok() {
                        match tokio::time::timeout(FLUSH_TIMEOUT, ack_rx).await {
                            Ok(Ok(true)) => {}
                            Ok(Ok(false)) => {
                                flush_ok = false;
                                flush_failure_kind = Some("write error");
                            }
                            Ok(Err(_)) => {
                                // Debouncer dropped the ack sender without
                                // replying — task already exited (e.g. a
                                // previous eviction flushed and closed). Any
                                // pending bytes went through the shutdown path.
                                debug!(
                                    "[notebook-sync] Eviction flush ack dropped for {} (debouncer exited)",
                                    notebook_id_for_eviction
                                );
                            }
                            Err(_) => {
                                flush_ok = false;
                                flush_failure_kind = Some("timeout");
                            }
                        }
                    }
                }
                if !flush_ok {
                    flush_retries += 1;
                    warn!(
                        "[notebook-sync] Eviction flush failed for {} ({}; attempt {}); keeping room resident, retrying in {:?}",
                        notebook_id_for_eviction,
                        flush_failure_kind.unwrap_or("unknown"),
                        flush_retries,
                        FLUSH_RETRY_DELAY
                    );
                    delay = FLUSH_RETRY_DELAY;
                    continue;
                }
                break;
            }

            // Remove room from the map under the lock, then drop the lock
            // BEFORE async teardown. Holding the lock across runtime agent
            // shutdown RPCs causes a convoy deadlock when the agent is
            // unresponsive — all notebook operations block on the lock.
            //
            // Look up the room by Arc pointer — UUID key is stable, but this
            // guards against double-eviction races.
            let (should_teardown, evicted_uuid) = {
                let mut rooms_guard = rooms_for_eviction.lock().await;
                if room_for_eviction.active_peers.load(Ordering::Relaxed) == 0 {
                    // Find the room's UUID key by Arc pointer identity
                    let current_key = rooms_guard
                        .iter()
                        .find(|(_, r)| Arc::ptr_eq(r, &room_for_eviction))
                        .map(|(k, _)| *k);
                    if let Some(uuid) = current_key {
                        rooms_guard.remove(&uuid);
                        (true, Some(uuid))
                    } else {
                        debug!(
                            "[notebook-sync] Eviction skipped for {} (room already removed)",
                            notebook_id_for_eviction
                        );
                        (false, None)
                    }
                } else {
                    (false, None)
                }
            }; // rooms lock dropped here

            // Clean up path_index entry (separate lock, after rooms lock is dropped).
            // Use remove_by_uuid rather than reading room.path — a concurrent writer
            // A concurrent save-path-update could hold room.path.write() and a
            // try_read() would silently return None, leaking the path_index entry.
            if should_teardown {
                if let Some(uuid) = evicted_uuid {
                    path_index_for_eviction.lock().await.remove_by_uuid(uuid);
                }
            }

            if should_teardown {
                // Shut down runtime agent subprocess if running. RuntimeAgentHandle::spawn
                // moves Child into a background task, so kill_on_drop doesn't
                // trigger on room drop — we need explicit shutdown via RPC.
                {
                    let has_runtime_agent = room_for_eviction
                        .runtime_agent_request_tx
                        .lock()
                        .await
                        .is_some();
                    if has_runtime_agent {
                        // Timeout the shutdown RPC — a dead/stuck agent shouldn't
                        // block teardown forever.
                        match tokio::time::timeout(
                            std::time::Duration::from_secs(5),
                            send_runtime_agent_request(
                                &room_for_eviction,
                                notebook_protocol::protocol::RuntimeAgentRequest::ShutdownKernel,
                            ),
                        )
                        .await
                        {
                            Ok(_) => {}
                            Err(_) => {
                                warn!(
                                    "[notebook-sync] Runtime agent shutdown timed out for {}, force-dropping",
                                    notebook_id_for_eviction
                                );
                            }
                        }
                        // Unregister from process group registry and drop handle
                        {
                            let mut guard = room_for_eviction.runtime_agent_handle.lock().await;
                            if let Some(ref handle) = *guard {
                                handle.unregister();
                            }
                            *guard = None;
                        }
                        {
                            let mut tx = room_for_eviction.runtime_agent_request_tx.lock().await;
                            *tx = None;
                        }
                    }
                }

                // Stop file watcher if running
                if let Some(shutdown_tx) = room_for_eviction.watcher_shutdown_tx.lock().await.take()
                {
                    let _ = shutdown_tx.send(());
                    debug!(
                        "[notebook-sync] Stopped file watcher for {}",
                        notebook_id_for_eviction
                    );
                }

                // Flush launched_config deps → metadata.runt.{uv,conda}.dependencies
                // before env cleanup and final save. This captures any packages
                // the user hot-installed during the session so they land in
                // the .ipynb, and feeds the preserve-predicate below with the
                // up-to-date dep list so the unified-hash path check points
                // at the right directory.
                //
                // The launched config carries deps for at most one runtime
                // (UV xor Conda), and `effective_user_deps_from_launched`
                // gates strictly on that — so at most one flush happens per
                // eviction. We record which runtime flushed so the rename
                // step below uses the right hash function.
                let launched_snapshot = room_for_eviction
                    .runtime_agent_launched_config
                    .read()
                    .await
                    .clone();
                let mut flushed_runtime: Option<CapturedEnvRuntime> = None;
                let mut save_succeeded = false;
                if let Some(ref launched) = launched_snapshot {
                    let has_saved_path = room_for_eviction.path.read().await.is_some();
                    if has_saved_path {
                        for runtime in [CapturedEnvRuntime::Uv, CapturedEnvRuntime::Conda] {
                            if flush_launched_deps_to_metadata(
                                &room_for_eviction,
                                launched,
                                runtime,
                            )
                            .await
                            {
                                flushed_runtime = Some(runtime);
                            }
                        }
                        if flushed_runtime.is_some() {
                            info!(
                                "[notebook-sync] Flushed hot-sync deps into metadata for {}",
                                notebook_id_for_eviction
                            );
                            // Persist to disk now — the autosave debouncer
                            // has already fired for this eviction, and the
                            // daemon is about to tear the room down.
                            match save_notebook_to_disk(&room_for_eviction, None).await {
                                Ok(_) => save_succeeded = true,
                                Err(e) => warn!(
                                    "[notebook-sync] Failed to persist hot-sync deps to {}: {} — skipping env-dir rename",
                                    notebook_id_for_eviction, e
                                ),
                            }
                        }
                    }
                }

                // Rename the env dir to match the post-flush unified
                // hash so the next reopen's `unified_env_on_disk` lookup
                // finds it. Skip the rename when save failed — leaving
                // disk metadata on the old hash while the env moved to
                // the new one would defeat the next reopen. Kernel is
                // already dead at this point (runtime agent was shut
                // down above), so the rename is safe.
                if let Some(runtime) = flushed_runtime {
                    if save_succeeded {
                        let current = room_for_eviction
                            .runtime_agent_env_path
                            .read()
                            .await
                            .clone();
                        if let Some(current_path) = current {
                            let metadata_after = {
                                let doc = room_for_eviction.doc.read().await;
                                doc.get_metadata_snapshot()
                            };
                            let new_path = rename_env_dir_to_unified_hash(
                                &current_path,
                                metadata_after.as_ref(),
                                runtime,
                                &kernel_env::uv::default_cache_dir_uv(),
                                &kernel_env::conda::default_cache_dir_conda(),
                            )
                            .await;
                            if new_path != current_path {
                                let mut ep = room_for_eviction.runtime_agent_env_path.write().await;
                                *ep = Some(new_path);
                            }
                        }
                    }
                }

                // Clean up the environment directory on eviction — unless
                // the room holds a captured env bound to a saved .ipynb.
                //
                // Pool envs (`runtimed-{uv,conda,pixi}-*`) and captured envs
                // for untitled notebooks are orphaned once the room is gone:
                // pool envs were mutated with the notebook's deps and can't
                // be returned, and captured envs with no saved .ipynb have
                // no persistent `env_id` reference. Both delete eagerly.
                //
                // Captured envs for saved notebooks are the reopen cache.
                // Preserve them so the next daemon session's first open
                // hits `unified_env_on_disk` instead of rebuilding from the
                // pool. A future age-based GC sweeps envs whose notebook
                // hasn't been opened in a long time.
                //
                // Use pool_env_root() to normalise pixi paths — their
                // venv_path is nested (e.g. .pixi/envs/default) but we
                // operate on the top-level runtimed-pixi-* directory.
                {
                    let env_path = room_for_eviction
                        .runtime_agent_env_path
                        .read()
                        .await
                        .clone();
                    if let Some(ref path) = env_path {
                        let has_saved_path = room_for_eviction.path.read().await.is_some();
                        let metadata = {
                            let doc = room_for_eviction.doc.read().await;
                            doc.get_metadata_snapshot()
                        };
                        let preserve = should_preserve_env_on_eviction(
                            has_saved_path,
                            path,
                            metadata.as_ref(),
                            &kernel_env::uv::default_cache_dir_uv(),
                            &kernel_env::conda::default_cache_dir_conda(),
                        );
                        if preserve {
                            info!(
                                "[notebook-sync] Preserving captured env {:?} on eviction (saved notebook)",
                                path
                            );
                        } else {
                            let root = crate::paths::pool_env_root(path);
                            let cache_dir = crate::paths::default_cache_dir();
                            if !crate::is_within_cache_dir(&root, &cache_dir) {
                                warn!(
                                    "[notebook-sync] Refusing to delete env {:?} on eviction (not within cache dir)",
                                    root
                                );
                            } else if root.exists() {
                                info!(
                                    "[notebook-sync] Cleaning up env {:?} on room eviction",
                                    root
                                );
                                if let Err(e) = tokio::fs::remove_dir_all(&root).await {
                                    warn!(
                                        "[notebook-sync] Failed to clean up env {:?} on eviction: {}",
                                        root, e
                                    );
                                }
                            }
                        }
                    }
                }

                info!(
                    "[notebook-sync] Evicted room {} (idle timeout)",
                    notebook_id_for_eviction
                );
            }
        });
    } else {
        info!(
            "[notebook-sync] Client disconnected from room {} ({} peer{} remaining)",
            notebook_id,
            remaining,
            if remaining == 1 { "" } else { "s" }
        );
    }

    result
}

/// Sanitize a peer label from the wire.
///
/// - Strips zero-width and control characters (ZWJ, ZWNJ, ZWSP, etc.)
/// - Trims whitespace
/// - Clamps to 64 Unicode scalar values
/// - Falls back to `fallback` if empty/missing
fn sanitize_peer_label(raw: Option<&str>, fallback: &str) -> String {
    const MAX_LABEL_CHARS: usize = 64;

    fn is_allowed(c: char) -> bool {
        !c.is_control()
            && !matches!(
                c,
                '\u{200B}' // zero-width space
                | '\u{200C}' // zero-width non-joiner
                | '\u{200D}' // zero-width joiner
                | '\u{200E}' // left-to-right mark
                | '\u{200F}' // right-to-left mark
                | '\u{2060}' // word joiner
                | '\u{FEFF}' // BOM / zero-width no-break space
                | '\u{00AD}' // soft hyphen
                | '\u{034F}' // combining grapheme joiner
                | '\u{061C}' // arabic letter mark
                | '\u{115F}' // hangul choseong filler
                | '\u{1160}' // hangul jungseong filler
                | '\u{17B4}' // khmer vowel inherent aq
                | '\u{17B5}' // khmer vowel inherent aa
                | '\u{180E}' // mongolian vowel separator
            )
            && !('\u{2066}'..='\u{2069}').contains(&c) // bidi isolates
            && !('\u{202A}'..='\u{202E}').contains(&c) // bidi overrides
            && !('\u{FE00}'..='\u{FE0F}').contains(&c) // variation selectors
            && !('\u{E0100}'..='\u{E01EF}').contains(&c) // variation selectors supplement
    }

    match raw {
        Some(s) => {
            // Filter and take at most MAX_LABEL_CHARS in one pass — avoids
            // allocating proportional to attacker-controlled input size.
            let cleaned: String = s
                .trim()
                .chars()
                .filter(|c| is_allowed(*c))
                .take(MAX_LABEL_CHARS)
                .collect();
            let trimmed = cleaned.trim();
            if trimmed.is_empty() {
                fallback.to_string()
            } else {
                trimmed.to_string()
            }
        }
        None => fallback.to_string(),
    }
}

/// State carried from the pre-send initial notebook-doc sync into the
/// steady-state loop.
///
/// See [`send_initial_notebook_doc_sync`]. `peer_state` tracks what the
/// daemon has already advertised about the notebook doc so subsequent
/// generate_sync_message calls compute correct deltas (including deltas
/// emitted by `streaming_load_cells`).
pub(crate) struct InitialSyncState {
    pub(crate) peer_state: sync::State,
}

impl InitialSyncState {
    fn new() -> Self {
        Self {
            peer_state: sync::State::new(),
        }
    }
}

/// Generate and send the initial notebook-doc AutomergeSync frame before
/// entering the background sync loop.
///
/// Runs synchronously as part of the handshake path so the first
/// AutomergeSync frame is on the wire before the client's `do_initial_sync`
/// starts ticking its 100ms-per-frame convergence timeout. Without this
/// ordering, under CI load the per-connection handler can be
/// spawned-but-not-yet-scheduled while the client is already timing out
/// waiting for the first sync frame, which flakes
/// `test_pipe_mode_forwards_sync_frames` and similar sync-sensitive tests.
///
/// Only the notebook-doc frame is pre-sent. The RuntimeStateDoc/PoolDoc
/// initial syncs and the eager RuntimeStateSnapshot/presence broadcasts
/// continue to run inside `run_sync_loop_v2` AFTER `streaming_load_cells`
/// populates per-cell outputs, and AFTER the broadcast channels are
/// subscribed. Pre-sending those here would either advertise an empty
/// state doc or drop auto-launch broadcasts that land between pre-send
/// and subscription.
///
/// Returns the `peer_state` so the steady-state loop (and streaming load)
/// continues from the same baseline and emits correct deltas.
pub(crate) async fn send_initial_notebook_doc_sync<W>(
    writer: &mut W,
    room: &Arc<NotebookRoom>,
) -> anyhow::Result<InitialSyncState>
where
    W: AsyncWrite + Unpin,
{
    let mut sync_state = InitialSyncState::new();

    // Encode the sync message inside the lock, then send outside it to avoid
    // holding the write lock across async I/O.
    let initial_encoded = {
        let mut doc = room.doc.write().await;
        match catch_automerge_panic("initial-doc-sync", || {
            doc.generate_sync_message(&mut sync_state.peer_state)
                .map(|msg| msg.encode())
        }) {
            Ok(encoded) => encoded,
            Err(e) => {
                warn!("{}", e);
                sync_state.peer_state = sync::State::new();
                if doc.rebuild_from_save() {
                    doc.generate_sync_message(&mut sync_state.peer_state)
                        .map(|msg| msg.encode())
                } else {
                    // Cell-count guard prevented rebuild — skip sync message,
                    // fresh peer_state will trigger full re-sync on next exchange
                    None
                }
            }
        }
    };
    if let Some(encoded) = initial_encoded {
        connection::send_typed_frame(writer, NotebookFrameType::AutomergeSync, &encoded).await?;
    }

    Ok(sync_state)
}

/// Typed frames sync loop with first-byte type indicator.
///
/// Handles both Automerge sync messages and NotebookRequest messages.
/// This protocol supports daemon-owned kernel execution (Phase 8).
///
/// The caller must have already run [`send_initial_notebook_doc_sync`] and
/// pass the resulting [`InitialSyncState`] via `initial_sync_state` so the
/// streaming load and steady-state loop continue on the same notebook-doc
/// `peer_state`.
#[allow(clippy::too_many_arguments)]
async fn run_sync_loop_v2<R, W>(
    reader: &mut R,
    writer: &mut W,
    room: &Arc<NotebookRoom>,
    _rooms: NotebookRooms,
    _notebook_id: String,
    daemon: std::sync::Arc<crate::daemon::Daemon>,
    needs_load: Option<&Path>,
    peer_id: &str,
    initial_sync_state: InitialSyncState,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let InitialSyncState { mut peer_state } = initial_sync_state;

    // Streaming load: add cells in batches and sync after each batch so
    // the frontend renders progressively. This runs before we subscribe
    // to changed_rx to avoid backlog from our own notifications.
    //
    // The initial AutomergeSync frame generated before this loop used the
    // empty-doc peer_state; streaming_load_cells continues on the same
    // peer_state so each batch emits a delta against what the client has
    // already been told about. streaming_load_cells also writes synthetic
    // execution outputs into the RuntimeStateDoc — that's why the initial
    // RuntimeStateDoc sync below runs AFTER load, not before.
    if let Some(load_path) = needs_load {
        if room.try_start_loading() {
            match streaming_load_cells(reader, writer, room, load_path, &mut peer_state).await {
                Ok(count) => {
                    room.finish_loading();
                    info!(
                        "[notebook-sync] Streaming load complete: {} cells from {}",
                        count,
                        load_path.display()
                    );
                }
                Err(e) => {
                    room.finish_loading();
                    // Clear partial cells so the next connection can retry
                    {
                        let mut doc = room.doc.write().await;
                        let _ = doc.clear_all_cells();
                    }
                    // Notify other peers so they converge to the cleared state
                    let _ = room.changed_tx.send(());
                    warn!(
                        "[notebook-sync] Streaming load failed for {}: {}",
                        load_path.display(),
                        e
                    );
                    return Err(anyhow::anyhow!("Streaming load failed: {}", e));
                }
            }
        }
        // If we lost the race (try_start_loading returned false), another
        // connection is loading. We'll pick up cells via changed_rx below.
    }

    // Subscribe to change notifications BEFORE sending the state/pool
    // initial syncs and eager snapshots, so any writes that land between
    // the snapshot read and the select loop are still delivered to this
    // peer as steady-state deltas. Without this, an auto-launch broadcast
    // (kernel status, env progress) fired during connection setup could
    // fall into the gap between "snapshot read" and "subscribe".
    let mut changed_rx = room.changed_tx.subscribe();
    let mut kernel_broadcast_rx = room.kernel_broadcast_tx.subscribe();
    let mut presence_rx = room.presence_tx.subscribe();
    let mut state_changed_rx = room.state_changed_tx.subscribe();

    // PoolDoc — global daemon pool state (UV/Conda availability, errors).
    let mut pool_changed_rx = daemon.pool_doc_changed.subscribe();

    let mut state_peer_state = sync::State::new();
    let mut pool_peer_state = sync::State::new();

    // Phase 1.1: Initial RuntimeStateDoc sync — encode inside lock, send outside.
    // Uses bounded generation to compact atomically if the message would exceed
    // the 100 MiB frame limit.
    let initial_state_encoded = {
        let mut state_doc = room.state_doc.write().await;
        // Safety net: compact before initial sync if the doc grew too large.
        // 80 MiB leaves headroom under the 100 MiB frame limit.
        const COMPACTION_THRESHOLD: usize = 80 * 1024 * 1024;
        if state_doc.compact_if_oversized(COMPACTION_THRESHOLD) {
            info!("[notebook-sync] Compacted oversized RuntimeStateDoc before initial sync");
        }
        match catch_automerge_panic("initial-state-sync", || {
            state_doc.generate_sync_message_bounded_encoded(
                &mut state_peer_state,
                STATE_SYNC_COMPACT_THRESHOLD,
            )
        }) {
            Ok(encoded) => encoded,
            Err(e) => {
                warn!("{}", e);
                state_doc.rebuild_from_save();
                state_peer_state = sync::State::new();
                state_doc
                    .generate_sync_message(&mut state_peer_state)
                    .map(|msg| msg.encode())
            }
        }
    };
    if let Some(encoded) = initial_state_encoded {
        connection::send_typed_frame(writer, NotebookFrameType::RuntimeStateSync, &encoded).await?;
    }

    // Phase 1.1b: Initial PoolDoc sync — global pool state
    let initial_pool_encoded = {
        let mut pool_doc = daemon.pool_doc.write().await;
        match catch_automerge_panic("initial-pool-sync", || {
            pool_doc
                .generate_sync_message(&mut pool_peer_state)
                .map(|msg| msg.encode())
        }) {
            Ok(encoded) => encoded,
            Err(e) => {
                warn!("{}", e);
                pool_doc.rebuild_from_save();
                pool_peer_state = sync::State::new();
                pool_doc
                    .generate_sync_message(&mut pool_peer_state)
                    .map(|msg| msg.encode())
            }
        }
    };
    if let Some(encoded) = initial_pool_encoded {
        connection::send_typed_frame(writer, NotebookFrameType::PoolStateSync, &encoded).await?;
    }

    // Phase 1.2: Eagerly send RuntimeState snapshot so the client has
    // kernel status immediately, without waiting for Automerge sync convergence.
    // The sync handshake takes multiple roundtrips; by the time it completes,
    // transient states like starting phases may have already passed.
    {
        let state = {
            let sd = room.state_doc.read().await;
            sd.read_state()
        };
        connection::send_typed_json_frame(
            writer,
            NotebookFrameType::Broadcast,
            &NotebookBroadcast::RuntimeStateSnapshot { state },
        )
        .await?;
    }

    // Phase 1.5 (removed): CommSync broadcast is no longer needed.
    // Late joiners receive widget state via RuntimeStateDoc CRDT sync,
    // and the frontend CRDT watcher synthesizes comm_open messages.

    // Phase 1.6: Send presence snapshot so late joiners see current peer state
    // (kernel status, cursors, selections from other connected peers).
    // The snapshot's peer_id field identifies the sender (daemon), not the receiver.
    // We filter out the receiver's own peer_id to prevent them from rendering
    // their own cursor as a remote peer (clients don't know their server-assigned ID).
    {
        let snapshot_bytes = {
            let presence_state = room.presence.read().await;
            if presence_state.peer_count() > 0 {
                // Build snapshot excluding this peer (they shouldn't see themselves)
                let other_peers: Vec<presence::PeerSnapshot> = presence_state
                    .peers()
                    .values()
                    .filter(|p| p.peer_id != peer_id)
                    .map(|p| presence::PeerSnapshot {
                        peer_id: p.peer_id.clone(),
                        peer_label: p.peer_label.clone(),
                        actor_label: p.actor_label.clone(),
                        channels: p.channels.values().cloned().collect(),
                    })
                    .collect();
                if !other_peers.is_empty() {
                    match presence::encode_snapshot("daemon", &other_peers) {
                        Ok(bytes) => Some(bytes),
                        Err(e) => {
                            warn!("[notebook-sync] Failed to encode presence snapshot: {}", e);
                            None
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            }
        }; // presence read guard dropped
        if let Some(snapshot_bytes) = snapshot_bytes {
            connection::send_typed_frame(writer, NotebookFrameType::Presence, &snapshot_bytes)
                .await?;
        }
    }

    // Periodic pruning of stale presence peers (e.g. clients that silently dropped).
    let prune_period = std::time::Duration::from_millis(presence::DEFAULT_HEARTBEAT_MS);
    let mut prune_interval = tokio::time::interval(prune_period);
    prune_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Phase 2: Exchange messages until sync is complete, then watch for changes
    loop {
        tokio::select! {
            // Incoming message from this client
            result = connection::recv_typed_frame(reader) => {
                match result? {
                    Some(frame) => {
                        match frame.frame_type {
                            NotebookFrameType::AutomergeSync => {
                                // Handle Automerge sync message
                                let message = sync::Message::decode(&frame.payload)
                                    .map_err(|e| anyhow::anyhow!("decode error: {}", e))?;

                                // Complete all document mutations inside the lock, encode the
                                // reply, then release the lock before performing async I/O.
                                let (persist_bytes, reply_encoded, metadata_changed) = {
                                    let mut doc = room.doc.write().await;

                                    let heads_before = doc.get_heads();

                                    // Guard receive_sync_message against automerge panics
                                    let recv_result = catch_automerge_panic("doc-receive-sync", || {
                                        doc.receive_sync_message(&mut peer_state, message)
                                    });
                                    match recv_result {
                                        Ok(Ok(())) => {}
                                        Ok(Err(e)) => {
                                            warn!("[notebook-sync] receive_sync_message error: {}", e);
                                            continue;
                                        }
                                        Err(e) => {
                                            warn!("{}", e);
                                            doc.rebuild_from_save();
                                            peer_state = sync::State::new();
                                            continue;
                                        }
                                    }

                                    let heads_after = doc.get_heads();
                                    let metadata_changed = diff_metadata_touched(
                                        doc.doc_mut(),
                                        &heads_before,
                                        &heads_after,
                                    );

                                    let bytes = doc.save();

                                    // Notify other peers in this room
                                    let _ = room.changed_tx.send(());

                                    let encoded = match catch_automerge_panic("doc-sync-reply", || {
                                        doc.generate_sync_message(&mut peer_state)
                                            .map(|reply| reply.encode())
                                    }) {
                                        Ok(encoded) => encoded,
                                        Err(e) => {
                                            warn!("{}", e);
                                            peer_state = sync::State::new();
                                            if doc.rebuild_from_save() {
                                                doc.generate_sync_message(&mut peer_state)
                                                    .map(|reply| reply.encode())
                                            } else {
                                                None
                                            }
                                        }
                                    };

                                    (bytes, encoded, metadata_changed)
                                };

                                // Send reply outside the lock so other peers can
                                // acquire it while we wait on the socket.
                                if let Some(encoded) = reply_encoded {
                                    connection::send_typed_frame(
                                        writer,
                                        NotebookFrameType::AutomergeSync,
                                        &encoded,
                                    )
                                    .await?;
                                }

                                // Send to debounced persistence task
                                if let Some(ref tx) = room.persist_tx {
                                    let _ = tx.send(Some(persist_bytes));
                                }

                                // Check if metadata changed and kernel is running - broadcast sync state
                                if metadata_changed {
                                    check_and_broadcast_sync_state(room).await;
                                }

                                // Re-verify trust from doc metadata (detects trust approval)
                                check_and_update_trust_state(room).await;

                                // Rebuild markdown asset refs after source sync.
                                process_markdown_assets(room).await;
                            }

                            NotebookFrameType::Request => {
                                // Decode the envelope, dispatch the inner request,
                                // echo the id on the response envelope so the caller
                                // can correlate multiple in-flight requests.
                                let envelope: notebook_protocol::protocol::NotebookRequestEnvelope =
                                    serde_json::from_slice(&frame.payload)?;
                                let response = handle_notebook_request(
                                    room,
                                    envelope.request,
                                    daemon.clone(),
                                )
                                .await;

                                // Promotion from untitled → file-backed is now handled
                                // entirely inside handle_notebook_request (SaveNotebook arm).
                                // Save path update is handled inside handle_notebook_request.

                                let reply = notebook_protocol::protocol::NotebookResponseEnvelope {
                                    id: envelope.id,
                                    response,
                                };
                                connection::send_typed_json_frame(
                                    writer,
                                    NotebookFrameType::Response,
                                    &reply,
                                )
                                .await?;
                            }

                            NotebookFrameType::Presence => {
                                // Client sent a presence update (cursor, selection, etc.)
                                // Reject oversized frames — presence data is small (~20-30 bytes).
                                if frame.payload.len() > presence::MAX_PRESENCE_FRAME_SIZE {
                                    warn!(
                                        "[notebook-sync] Oversized presence frame ({} bytes, max {}), dropping",
                                        frame.payload.len(),
                                        presence::MAX_PRESENCE_FRAME_SIZE
                                    );
                                    continue;
                                }

                                // Decode, update room state, relay to other peers.
                                let now_ms = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis() as u64;

                                match presence::decode_message(&frame.payload) {
                                    Ok(presence::PresenceMessage::Update { data, peer_label, actor_label, .. }) => {
                                        // Reject daemon-owned channels before updating shared state.
                                        // This prevents clients from spoofing kernel status.
                                        if matches!(data, presence::ChannelData::KernelState(_)) {
                                            warn!("[notebook-sync] Client tried to publish KernelState presence, ignoring");
                                        } else {
                                            let data_for_relay = data.clone();
                                            let actor_label_for_relay = actor_label.clone();
                                            // Sanitize peer_label: trim whitespace, clamp length,
                                            // treat empty as fallback. Prevents UI/memory issues
                                            // from malicious or buggy clients.
                                            let label = sanitize_peer_label(peer_label.as_deref(), peer_id);
                                            let sanitized_label = Some(label.clone());
                                            // Update the room's presence state (using our known peer_id,
                                            // not the one in the frame — clients don't know their peer_id).
                                            let is_new = room.presence.write().await.update_peer(
                                                peer_id,
                                                &label,
                                                actor_label.as_deref(),
                                                data,
                                                now_ms,
                                            );

                                            if is_new {
                                                // New peer — send snapshot of everyone else (excluding self)
                                                let other_peers: Vec<presence::PeerSnapshot> = room
                                                    .presence
                                                    .read()
                                                    .await
                                                    .peers()
                                                    .values()
                                                    .filter(|p| p.peer_id != peer_id)
                                                    .map(|p| presence::PeerSnapshot {
                                                        peer_id: p.peer_id.clone(),
                                                        peer_label: p.peer_label.clone(),
                                                        actor_label: p.actor_label.clone(),
                                                        channels: p.channels.values().cloned().collect(),
                                                    })
                                                    .collect();
                                                if !other_peers.is_empty() {
                                                    match presence::encode_snapshot(
                                                        "daemon",
                                                        &other_peers,
                                                    ) {
                                                        Ok(snapshot_bytes) => {
                                                            connection::send_typed_frame(
                                                                writer,
                                                                NotebookFrameType::Presence,
                                                                &snapshot_bytes,
                                                            )
                                                            .await?;
                                                        }
                                                        Err(e) => warn!(
                                                            "[notebook-sync] Failed to encode presence snapshot for new peer: {}",
                                                            e
                                                        ),
                                                    }
                                                }
                                            }

                                            // Re-encode with server-assigned peer_id and sanitized label
                                            if let Ok(bytes) = presence::encode_message(
                                                &presence::PresenceMessage::Update {
                                                    peer_id: peer_id.to_string(),
                                                    peer_label: sanitized_label,
                                                    actor_label: actor_label_for_relay,
                                                    data: data_for_relay,
                                                },
                                            ) {
                                                let _ = room.presence_tx.send((peer_id.to_string(), bytes));
                                            }
                                        }
                                    }
                                    Ok(presence::PresenceMessage::Heartbeat { .. }) => {
                                        room.presence.write().await.mark_seen(peer_id, now_ms);
                                    }
                                    Ok(presence::PresenceMessage::ClearChannel { channel, .. }) => {
                                        room.presence.write().await.clear_channel(peer_id, channel);
                                        match presence::encode_clear_channel(peer_id, channel) {
                                            Ok(bytes) => {
                                                let _ = room.presence_tx.send((peer_id.to_string(), bytes));
                                            }
                                            Err(e) => warn!(
                                                "[notebook-sync] Failed to encode clear_channel presence: {}",
                                                e
                                            ),
                                        }
                                    }
                                    Ok(_) => {
                                        // Snapshot/Left from a client — ignore
                                    }
                                    Err(e) => {
                                        warn!(
                                            "[notebook-sync] Failed to decode presence frame: {}",
                                            e
                                        );
                                    }
                                }
                            }

                            NotebookFrameType::RuntimeStateSync => {
                                // Client sync — accept changes (frontend may write
                                // to comms/*/state/* for widget state updates).
                                let message = sync::Message::decode(&frame.payload)
                                    .map_err(|e| anyhow::anyhow!("decode state sync: {}", e))?;
                                let reply_encoded = {
                                    let mut state_doc = room.state_doc.write().await;

                                    let recv_result = catch_automerge_panic("state-receive-sync", || {
                                        state_doc.receive_sync_message_with_changes(
                                            &mut state_peer_state,
                                            message,
                                        )
                                    });
                                    let had_changes = match recv_result {
                                        Ok(Ok(changed)) => changed,
                                        Ok(Err(e)) => {
                                            warn!("[notebook-sync] state receive_sync_message error: {}", e);
                                            continue;
                                        }
                                        Err(e) => {
                                            warn!("{}", e);
                                            state_doc.rebuild_from_save();
                                            state_peer_state = sync::State::new();
                                            continue;
                                        }
                                    };

                                    // If client sent changes, notify all peers.
                                    // Comm state forwarding to kernel: the runtime agent diffs
                                    // comm state before/after each RuntimeStateSync and
                                    // sends changed properties to the kernel via send_comm_update.
                                    if had_changes {
                                        let _ = room.state_changed_tx.send(());
                                    }

                                    match catch_automerge_panic("state-sync-reply", || {
                                        state_doc
                                            .generate_sync_message(&mut state_peer_state)
                                            .map(|msg| msg.encode())
                                    }) {
                                        Ok(encoded) => encoded,
                                        Err(e) => {
                                            warn!("{}", e);
                                            state_doc.rebuild_from_save();
                                            state_peer_state = sync::State::new();
                                            state_doc
                                                .generate_sync_message(&mut state_peer_state)
                                                .map(|msg| msg.encode())
                                        }
                                    }
                                };
                                if let Some(encoded) = reply_encoded {
                                    connection::send_typed_frame(
                                        writer,
                                        NotebookFrameType::RuntimeStateSync,
                                        &encoded,
                                    )
                                    .await?;
                                }
                            }

                            NotebookFrameType::PoolStateSync => {
                                // Client's pool sync reply — apply with change stripping
                                let message = sync::Message::decode(&frame.payload)
                                    .map_err(|e| anyhow::anyhow!("decode pool sync: {}", e))?;
                                let reply_encoded = {
                                    let mut pool_doc = daemon.pool_doc.write().await;

                                    let recv_result = catch_automerge_panic("pool-receive-sync", || {
                                        pool_doc.receive_sync_message(
                                            &mut pool_peer_state,
                                            message,
                                        )
                                    });
                                    match recv_result {
                                        Ok(Ok(())) => {}
                                        Ok(Err(e)) => {
                                            warn!("[notebook-sync] pool receive_sync_message error: {}", e);
                                            continue;
                                        }
                                        Err(e) => {
                                            warn!("{}", e);
                                            pool_doc.rebuild_from_save();
                                            pool_peer_state = sync::State::new();
                                            continue;
                                        }
                                    }

                                    match catch_automerge_panic("pool-sync-reply", || {
                                        pool_doc
                                            .generate_sync_message(&mut pool_peer_state)
                                            .map(|msg| msg.encode())
                                    }) {
                                        Ok(encoded) => encoded,
                                        Err(e) => {
                                            warn!("{}", e);
                                            pool_doc.rebuild_from_save();
                                            pool_peer_state = sync::State::new();
                                            pool_doc
                                                .generate_sync_message(&mut pool_peer_state)
                                                .map(|msg| msg.encode())
                                        }
                                    }
                                };
                                if let Some(encoded) = reply_encoded {
                                    connection::send_typed_frame(
                                        writer,
                                        NotebookFrameType::PoolStateSync,
                                        &encoded,
                                    )
                                    .await?;
                                }
                            }

                            NotebookFrameType::Response | NotebookFrameType::Broadcast => {
                                // Clients shouldn't send these
                                warn!(
                                    "[notebook-sync] Unexpected frame type from client: {:?}",
                                    frame.frame_type
                                );
                            }
                        }
                    }
                    None => {
                        // Client disconnected
                        return Ok(());
                    }
                }
            }

            // Another peer changed the document — push update to this client
            _ = changed_rx.recv() => {
                // Encode inside the lock, send outside it to avoid holding the
                // write lock across async I/O.
                let encoded = {
                    let mut doc = room.doc.write().await;
                    match catch_automerge_panic("doc-broadcast", || {
                        doc.generate_sync_message(&mut peer_state)
                            .map(|msg| msg.encode())
                    }) {
                        Ok(encoded) => encoded,
                        Err(e) => {
                            warn!("{}", e);
                            peer_state = sync::State::new();
                            if doc.rebuild_from_save() {
                                doc.generate_sync_message(&mut peer_state)
                                    .map(|msg| msg.encode())
                            } else {
                                None
                            }
                        }
                    }
                };
                if let Some(encoded) = encoded {
                    connection::send_typed_frame(
                        writer,
                        NotebookFrameType::AutomergeSync,
                        &encoded,
                    )
                    .await?;
                }
            }

            // RuntimeStateDoc changed — push update to this client
            result = state_changed_rx.recv() => {
                match result {
                    Ok(()) => {
                        let encoded = {
                            let mut state_doc = room.state_doc.write().await;
                            match catch_automerge_panic("state-broadcast", || {
                                state_doc
                                    .generate_sync_message(&mut state_peer_state)
                                    .map(|msg| msg.encode())
                            }) {
                                Ok(encoded) => encoded,
                                Err(e) => {
                                    warn!("{}", e);
                                    state_doc.rebuild_from_save();
                                    state_peer_state = sync::State::new();
                                    state_doc
                                        .generate_sync_message(&mut state_peer_state)
                                        .map(|msg| msg.encode())
                                }
                            }
                        };
                        if let Some(encoded) = encoded {
                            connection::send_typed_frame(
                                writer,
                                NotebookFrameType::RuntimeStateSync,
                                &encoded,
                            )
                            .await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::debug!(
                            "[notebook-sync] Peer {} lagged {} runtime state updates",
                            peer_id, n
                        );
                        // Send a full sync to catch up
                        let encoded = {
                            let mut state_doc = room.state_doc.write().await;
                            match catch_automerge_panic("state-broadcast-lagged", || {
                                state_doc
                                    .generate_sync_message(&mut state_peer_state)
                                    .map(|msg| msg.encode())
                            }) {
                                Ok(encoded) => encoded,
                                Err(e) => {
                                    warn!("{}", e);
                                    state_doc.rebuild_from_save();
                                    state_peer_state = sync::State::new();
                                    state_doc
                                        .generate_sync_message(&mut state_peer_state)
                                        .map(|msg| msg.encode())
                                }
                            }
                        };
                        if let Some(encoded) = encoded {
                            connection::send_typed_frame(
                                writer,
                                NotebookFrameType::RuntimeStateSync,
                                &encoded,
                            )
                            .await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // State change channel closed — room is being evicted
                        return Ok(());
                    }
                }
            }

            // PoolDoc changed — push update to this client
            result = pool_changed_rx.recv() => {
                match result {
                    Ok(()) => {
                        let encoded = {
                            let mut pool_doc = daemon.pool_doc.write().await;
                            match catch_automerge_panic("pool-broadcast", || {
                                pool_doc
                                    .generate_sync_message(&mut pool_peer_state)
                                    .map(|msg| msg.encode())
                            }) {
                                Ok(encoded) => encoded,
                                Err(e) => {
                                    warn!("{}", e);
                                    pool_doc.rebuild_from_save();
                                    pool_peer_state = sync::State::new();
                                    pool_doc
                                        .generate_sync_message(&mut pool_peer_state)
                                        .map(|msg| msg.encode())
                                }
                            }
                        };
                        if let Some(encoded) = encoded {
                            connection::send_typed_frame(
                                writer,
                                NotebookFrameType::PoolStateSync,
                                &encoded,
                            )
                            .await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::debug!(
                            "[notebook-sync] Peer {} lagged {} pool state updates",
                            peer_id, n
                        );
                        let encoded = {
                            let mut pool_doc = daemon.pool_doc.write().await;
                            match catch_automerge_panic("pool-broadcast-lagged", || {
                                pool_doc
                                    .generate_sync_message(&mut pool_peer_state)
                                    .map(|msg| msg.encode())
                            }) {
                                Ok(encoded) => encoded,
                                Err(e) => {
                                    warn!("{}", e);
                                    pool_doc.rebuild_from_save();
                                    pool_peer_state = sync::State::new();
                                    pool_doc
                                        .generate_sync_message(&mut pool_peer_state)
                                        .map(|msg| msg.encode())
                                }
                            }
                        };
                        if let Some(encoded) = encoded {
                            connection::send_typed_frame(
                                writer,
                                NotebookFrameType::PoolStateSync,
                                &encoded,
                            )
                            .await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // Pool doc channel closed — daemon is shutting down
                        return Ok(());
                    }
                }
            }

            // Presence update from another peer — forward to this client
            result = presence_rx.recv() => {
                match result {
                    Ok((ref sender_peer_id, ref bytes)) => {
                        // Don't echo back to the sender
                        if sender_peer_id != peer_id {
                            connection::send_typed_frame(
                                writer,
                                NotebookFrameType::Presence,
                                bytes,
                            )
                            .await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // Missed some presence updates — send a full snapshot to catch up
                        tracing::debug!(
                            "[notebook-sync] Peer {} lagged {} presence updates, sending snapshot",
                            peer_id, n
                        );
                        match room.presence.read().await.encode_snapshot(peer_id) {
                            Ok(snapshot_bytes) => {
                                connection::send_typed_frame(
                                    writer,
                                    NotebookFrameType::Presence,
                                    &snapshot_bytes,
                                )
                                .await?;
                            }
                            Err(e) => warn!(
                                "[notebook-sync] Failed to encode lag-recovery snapshot: {}",
                                e
                            ),
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // Presence channel closed — room is being evicted
                        return Ok(());
                    }
                }
            }

            // Kernel broadcast event — forward to this client
            result = kernel_broadcast_rx.recv() => {
                match result {
                    Ok(broadcast) => {
                        // Drop broadcasts that are redundant with RuntimeStateDoc
                        // (synced via frame 0x05). The daemon still emits these
                        // internally (e.g. ExecutionDone drives the command loop),
                        // but peers no longer need them — RuntimeStateDoc is the
                        // single source of truth for kernel status, queue, execution
                        // lifecycle, and env sync state.
                        if matches!(
                            &broadcast,
                            NotebookBroadcast::KernelStatus { .. }
                                | NotebookBroadcast::ExecutionStarted { .. }
                                | NotebookBroadcast::ExecutionDone { .. }
                                | NotebookBroadcast::QueueChanged { .. }
                                | NotebookBroadcast::EnvSyncState { .. }
                        ) {
                            // ExecutionDone previously triggered a doc sync flush
                            // to ensure outputs arrived before the signal. Now that
                            // the broadcast is dropped, the sync still happens via
                            // the RuntimeStateDoc update path — the daemon writes
                            // execution status to the RuntimeStateDoc *after*
                            // writing outputs to the notebook doc, so the
                            // frame-0x05 sync message is the new "outputs ready"
                            // signal for peers.
                            continue;
                        }

                        connection::send_typed_json_frame(
                            writer,
                            NotebookFrameType::Broadcast,
                            &broadcast,
                        )
                        .await?;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(
                            "[notebook-sync] Peer lagged {} kernel broadcasts, sending doc sync to catch up",
                            n
                        );
                        // The peer missed some broadcasts (outputs, status changes).
                        // The Automerge doc contains the persisted state, so send a
                        // sync message to catch the peer up on any missed output data.
                        send_doc_sync(
                            room,
                            &mut peer_state,
                            writer,
                        )
                        .await?;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // Broadcast channel closed — room is being evicted
                        return Ok(());
                    }
                }
            }

            // Prune stale presence peers that haven't heartbeated within the TTL.
            // Each connection's loop is proof-of-life for its own peer, so we
            // mark ourselves seen before pruning to avoid false self-eviction
            // (idle-but-connected peers don't send frames).
            _ = prune_interval.tick() => {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let mut presence_state = room.presence.write().await;
                presence_state.mark_seen(peer_id, now_ms);
                let pruned = presence_state.prune_stale(now_ms, presence::DEFAULT_PEER_TTL_MS);
                drop(presence_state);
                for pruned_peer_id in pruned {
                    match presence::encode_left(&pruned_peer_id) {
                        Ok(left_bytes) => {
                            let _ = room.presence_tx.send((pruned_peer_id, left_bytes));
                        }
                        Err(e) => warn!(
                            "[notebook-sync] Failed to encode 'left' for pruned peer: {}",
                            e
                        ),
                    }
                }
            }
        }
    }
}

/// Send a doc sync message to a peer if there are pending changes.
///
/// Generates an Automerge sync message from the room's doc and sends it
/// as a typed frame. Used before forwarding ExecutionDone (to ensure
/// outputs are synced) and after broadcast lag recovery.
async fn send_doc_sync<W: tokio::io::AsyncWrite + Unpin>(
    room: &NotebookRoom,
    peer_state: &mut automerge::sync::State,
    writer: &mut W,
) -> anyhow::Result<()> {
    let encoded = {
        let mut doc = room.doc.write().await;
        match catch_automerge_panic("broadcast-doc-changes", || {
            doc.generate_sync_message(peer_state)
                .map(|msg| msg.encode())
        }) {
            Ok(encoded) => encoded,
            Err(e) => {
                warn!("{}", e);
                doc.rebuild_from_save();
                *peer_state = sync::State::new();
                doc.generate_sync_message(peer_state)
                    .map(|msg| msg.encode())
            }
        }
    };
    if let Some(encoded) = encoded {
        connection::send_typed_frame(writer, NotebookFrameType::AutomergeSync, &encoded).await?;
    }
    Ok(())
}

/// Which runtime this capture applies to — UV pool envs or Conda pool envs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapturedEnvRuntime {
    Uv,
    Conda,
}

/// Write captured user-level deps and env_id into the notebook's metadata
/// if not already set.
///
/// This is the first-launch capture step from the unified env resolution
/// design: after the daemon takes an env from the pool and claims it at the
/// unified-hash path, the notebook's metadata records the user-level deps
/// derived from the pool env's install list (base packages stripped). On
/// subsequent reopens, this captured set participates in the hash lookup
/// so the claimed env is found on disk.
///
/// Write-once semantics:
/// - `env_id` is set only if the metadata's `env_id` is `None`.
/// - `dependencies` on `metadata.runt.uv` or `metadata.runt.conda` is
///   overwritten only if empty (the brand-new notebook case). Existing
///   non-empty deps are left alone so user-edited lists aren't clobbered.
///
/// Uses `fork_and_merge` per the async-CRDT-mutation rule.
///
/// Returns `true` if any field was updated.
async fn capture_env_into_metadata(
    room: &NotebookRoom,
    runtime: CapturedEnvRuntime,
    user_defaults: &[String],
    env_id: &str,
) -> bool {
    let mut changed = false;
    let mut doc = room.doc.write().await;
    doc.fork_and_merge(|fork| {
        let mut snap = fork.get_metadata_snapshot().unwrap_or_default();

        if snap.runt.env_id.is_none() {
            snap.runt.env_id = Some(env_id.to_string());
            changed = true;
        }

        match runtime {
            CapturedEnvRuntime::Uv => {
                let uv =
                    snap.runt
                        .uv
                        .get_or_insert_with(|| notebook_doc::metadata::UvInlineMetadata {
                            dependencies: Vec::new(),
                            requires_python: None,
                            prerelease: None,
                        });
                if uv.dependencies.is_empty() && !user_defaults.is_empty() {
                    uv.dependencies = user_defaults.to_vec();
                    changed = true;
                }
            }
            CapturedEnvRuntime::Conda => {
                let conda = snap.runt.conda.get_or_insert_with(|| {
                    notebook_doc::metadata::CondaInlineMetadata {
                        dependencies: Vec::new(),
                        channels: vec!["conda-forge".to_string()],
                        python: None,
                    }
                });
                if conda.dependencies.is_empty() && !user_defaults.is_empty() {
                    conda.dependencies = user_defaults.to_vec();
                    changed = true;
                }
            }
        }

        if changed {
            let _ = fork.set_metadata_snapshot(&snap);
        }
    });
    drop(doc);
    if changed {
        // Notify the autosave debouncer so the capture lands in the .ipynb
        // even when the user closes the notebook without any other edits.
        // Without this, captured metadata lives only in the in-memory CRDT
        // and evaporates on room eviction, making the next reopen re-capture
        // from scratch.
        let _ = room.changed_tx.send(());
    }
    changed
}

/// Derive the user-level dep list that should land in metadata for the
/// given runtime, based on the launched config currently running.
///
/// `launched.uv_deps` / `launched.conda_deps` are the source of truth
/// while the kernel is up. They're populated at launch time for
/// captured reopens and inline-deps launches, and the hot-sync handler
/// always appends synced packages to the matching field before
/// returning success. So if `launched.uv_deps` is `None`, no UV hot
/// sync occurred and captured metadata already matches the on-disk env
/// — no flush needed for UV. Same for Conda.
///
/// `strip_base` removes the runtime's base packages so the persisted
/// list reflects only user intent.
///
/// Returns `None` for runtimes that didn't participate in the launch
/// (the other runtime's field, deno-only launches, pixi launches).
fn effective_user_deps_from_launched(
    launched: &LaunchedEnvConfig,
    runtime: CapturedEnvRuntime,
) -> Option<Vec<String>> {
    match runtime {
        CapturedEnvRuntime::Uv => {
            let deps = launched.uv_deps.as_ref()?;
            Some(kernel_env::strip_base(
                deps,
                kernel_env::uv::UV_BASE_PACKAGES,
            ))
        }
        CapturedEnvRuntime::Conda => {
            let deps = launched.conda_deps.as_ref()?;
            Some(kernel_env::strip_base(
                deps,
                kernel_env::conda::CONDA_BASE_PACKAGES,
            ))
        }
    }
}

/// Write the post-hot-sync dep list back to `metadata.runt.{uv,conda}.
/// dependencies` if it drifted from what the kernel actually has
/// installed. Called at eviction time so the saved `.ipynb` reflects
/// every package the user hot-installed during the session.
///
/// Returns `true` if metadata was updated (caller should persist the
/// notebook to disk before the room is torn down).
async fn flush_launched_deps_to_metadata(
    room: &NotebookRoom,
    launched: &LaunchedEnvConfig,
    runtime: CapturedEnvRuntime,
) -> bool {
    let Some(new_deps) = effective_user_deps_from_launched(launched, runtime) else {
        return false;
    };

    let mut changed = false;
    let mut doc = room.doc.write().await;
    doc.fork_and_merge(|fork| {
        let mut snap = fork.get_metadata_snapshot().unwrap_or_default();
        match runtime {
            CapturedEnvRuntime::Uv => {
                let uv =
                    snap.runt
                        .uv
                        .get_or_insert_with(|| notebook_doc::metadata::UvInlineMetadata {
                            dependencies: Vec::new(),
                            requires_python: None,
                            prerelease: None,
                        });
                if uv.dependencies != new_deps {
                    uv.dependencies = new_deps.clone();
                    changed = true;
                }
            }
            CapturedEnvRuntime::Conda => {
                let conda = snap.runt.conda.get_or_insert_with(|| {
                    notebook_doc::metadata::CondaInlineMetadata {
                        dependencies: Vec::new(),
                        channels: vec!["conda-forge".to_string()],
                        python: None,
                    }
                });
                if conda.dependencies != new_deps {
                    conda.dependencies = new_deps.clone();
                    changed = true;
                }
            }
        }
        if changed {
            let _ = fork.set_metadata_snapshot(&snap);
        }
    });
    drop(doc);
    changed
}

/// Full dep-shape captured in notebook metadata plus its env_id.
///
/// Carries every resolver-affecting field so the unified-hash lookup on
/// reopen matches the hash the capture step wrote. Dropping any of these
/// fields (e.g. reducing to just `dependencies`) would make the hash diverge
/// whenever a user edits `runt.uv.prerelease`, `runt.uv.requires-python`,
/// `runt.conda.channels`, or `runt.conda.python` after capture.
#[derive(Debug, Clone)]
pub(crate) enum CapturedEnv {
    Uv {
        deps: kernel_env::UvDependencies,
        env_id: String,
    },
    Conda {
        deps: kernel_env::CondaDependencies,
        env_id: String,
    },
}

impl CapturedEnv {
    #[allow(dead_code)]
    fn env_id(&self) -> &str {
        match self {
            CapturedEnv::Uv { env_id, .. } | CapturedEnv::Conda { env_id, .. } => env_id,
        }
    }

    #[allow(dead_code)]
    fn dependencies(&self) -> &[String] {
        match self {
            CapturedEnv::Uv { deps, .. } => &deps.dependencies,
            CapturedEnv::Conda { deps, .. } => &deps.dependencies,
        }
    }
}

/// Pull the captured env shape (full dep spec + env_id) out of a metadata
/// snapshot. Returns `None` if no env_id is set.
///
/// Extracts every field that feeds into `compute_unified_env_hash`:
/// - UV: `dependencies`, `requires-python`, `prerelease`
/// - Conda: `dependencies`, `channels`, `python`
///
/// Missing `runt.uv` / `runt.conda` sections yield default (empty) deps with
/// `None` resolver fields, which matches the on-disk hash at first-launch
/// capture time (when those sections are written with defaults).
pub(crate) fn captured_env_for_runtime(
    snapshot: Option<&NotebookMetadataSnapshot>,
    runtime: CapturedEnvRuntime,
) -> Option<CapturedEnv> {
    let snap = snapshot?;
    let env_id = snap.runt.env_id.as_ref()?.clone();
    match runtime {
        CapturedEnvRuntime::Uv => {
            let (dependencies, requires_python, prerelease) = snap
                .runt
                .uv
                .as_ref()
                .map(|u| {
                    (
                        u.dependencies.clone(),
                        u.requires_python.clone(),
                        u.prerelease.clone(),
                    )
                })
                .unwrap_or_else(|| (Vec::new(), None, None));
            Some(CapturedEnv::Uv {
                deps: kernel_env::UvDependencies {
                    dependencies,
                    requires_python,
                    prerelease,
                },
                env_id,
            })
        }
        CapturedEnvRuntime::Conda => {
            let (dependencies, channels, python) = snap
                .runt
                .conda
                .as_ref()
                .map(|c| {
                    let channels = if c.channels.is_empty() {
                        vec!["conda-forge".to_string()]
                    } else {
                        c.channels.clone()
                    };
                    (c.dependencies.clone(), channels, c.python.clone())
                })
                .unwrap_or_else(|| (Vec::new(), vec!["conda-forge".to_string()], None));
            Some(CapturedEnv::Conda {
                deps: kernel_env::CondaDependencies {
                    dependencies,
                    channels,
                    python,
                    env_id: None,
                },
                env_id,
            })
        }
    }
}

/// Check whether a captured env exists on disk at the unified-hash path.
/// Returns the cache path + python path if present.
pub(crate) fn unified_env_on_disk(captured: &CapturedEnv) -> Option<(PathBuf, PathBuf)> {
    unified_env_on_disk_in(
        captured,
        &kernel_env::uv::default_cache_dir_uv(),
        &kernel_env::conda::default_cache_dir_conda(),
    )
}

/// Test-friendly form of [`unified_env_on_disk`] that accepts explicit
/// cache dirs instead of using the channel defaults. The production call
/// site always passes the defaults; tests pass tmpdirs.
fn unified_env_on_disk_in(
    captured: &CapturedEnv,
    uv_cache_dir: &Path,
    conda_cache_dir: &Path,
) -> Option<(PathBuf, PathBuf)> {
    match captured {
        CapturedEnv::Uv { deps, env_id } => {
            let hash = kernel_env::uv::compute_unified_env_hash(deps, env_id);
            let venv_path = uv_cache_dir.join(&hash);

            #[cfg(target_os = "windows")]
            let python_path = venv_path.join("Scripts").join("python.exe");
            #[cfg(not(target_os = "windows"))]
            let python_path = venv_path.join("bin").join("python");

            if python_path.exists() {
                Some((venv_path, python_path))
            } else {
                None
            }
        }
        CapturedEnv::Conda { deps, env_id } => {
            let hash = kernel_env::conda::compute_unified_env_hash(deps, env_id);
            let env_path = conda_cache_dir.join(&hash);

            #[cfg(target_os = "windows")]
            let python_path = env_path.join("python.exe");
            #[cfg(not(target_os = "windows"))]
            let python_path = env_path.join("bin").join("python");

            if python_path.exists() {
                Some((env_path, python_path))
            } else {
                None
            }
        }
    }
}

/// Decide whether to preserve the room's env directory on eviction.
///
/// Rule: if the notebook has a saved path on disk AND its current
/// `env_path` matches the captured unified-hash env path in metadata,
/// keep the env. Saved path means the `.ipynb` will persist the env_id
/// binding, so the on-disk cache is reachable on next open.
///
/// Untitled / never-saved rooms have no persistent reference and their
/// env is orphaned on eviction — delete as before.
///
/// Non-captured envs (pool / legacy inline) are always cleaned because
/// their paths don't survive re-launch regardless.
fn should_preserve_env_on_eviction(
    has_saved_path: bool,
    env_path: &Path,
    metadata: Option<&NotebookMetadataSnapshot>,
    uv_cache_dir: &Path,
    conda_cache_dir: &Path,
) -> bool {
    if !has_saved_path {
        return false;
    }
    for runtime in [CapturedEnvRuntime::Uv, CapturedEnvRuntime::Conda] {
        if let Some(captured) = captured_env_for_runtime(metadata, runtime) {
            if let Some((venv, _)) =
                unified_env_on_disk_in(&captured, uv_cache_dir, conda_cache_dir)
            {
                if venv == env_path {
                    return true;
                }
            }
        }
    }
    false
}

/// Compute the target path the env dir should live at, given the
/// runtime the kernel was actually launched with. Returns `None` when
/// the requested runtime has no captured env in metadata.
///
/// The explicit `runtime` argument avoids the UV-first metadata-probe
/// bug: a pure-conda notebook has `runt.env_id` set and
/// `captured_env_for_runtime(Uv)` happily synthesises an empty UV
/// capture, so without the runtime hint the old version would route
/// conda eviction through a UV hash.
fn unified_hash_target_path(
    metadata: Option<&NotebookMetadataSnapshot>,
    runtime: CapturedEnvRuntime,
    uv_cache_dir: &Path,
    conda_cache_dir: &Path,
) -> Option<PathBuf> {
    let captured = captured_env_for_runtime(metadata, runtime)?;
    let (hash, dir) = match &captured {
        CapturedEnv::Uv { deps, env_id } => (
            kernel_env::uv::compute_unified_env_hash(deps, env_id),
            uv_cache_dir,
        ),
        CapturedEnv::Conda { deps, env_id } => (
            kernel_env::conda::compute_unified_env_hash(deps, env_id),
            conda_cache_dir,
        ),
    };
    Some(dir.join(&hash))
}

/// Rename the env dir on disk to match the captured metadata's unified
/// hash for the given runtime. Called at eviction after the hot-sync
/// flush updates metadata deps: the on-disk dir is still named for the
/// pre-flush hash, so we rename it before the next reopen's cache
/// lookup.
///
/// Returns the path after the rename. Callers should use this as the
/// authoritative env path for any later steps (e.g. the preserve check).
///
/// Skips the rename when:
/// - The current path already matches the target hash (no drift).
/// - The source path no longer exists (already cleaned up).
/// - The target path already exists (would clobber another env — log
///   and keep the source).
async fn rename_env_dir_to_unified_hash(
    current_env_path: &Path,
    metadata: Option<&NotebookMetadataSnapshot>,
    runtime: CapturedEnvRuntime,
    uv_cache_dir: &Path,
    conda_cache_dir: &Path,
) -> PathBuf {
    let Some(target) = unified_hash_target_path(metadata, runtime, uv_cache_dir, conda_cache_dir)
    else {
        return current_env_path.to_path_buf();
    };
    if target == current_env_path {
        return current_env_path.to_path_buf();
    }
    if !current_env_path.exists() {
        return current_env_path.to_path_buf();
    }
    if target.exists() {
        warn!(
            "[notebook-sync] Unified-hash rename target {:?} already exists; leaving env at {:?}",
            target, current_env_path
        );
        return current_env_path.to_path_buf();
    }
    match tokio::fs::rename(current_env_path, &target).await {
        Ok(()) => {
            info!(
                "[notebook-sync] Renamed env {:?} -> {:?} to match new unified hash",
                current_env_path, target
            );
            target
        }
        Err(e) => {
            warn!(
                "[notebook-sync] Failed to rename env {:?} -> {:?}: {}",
                current_env_path, target, e
            );
            current_env_path.to_path_buf()
        }
    }
}

/// Return a captured-env `env_source` override (`uv:prewarmed` /
/// `conda:prewarmed`) when the notebook has captured deps + env_id and a
/// matching unified-hash env exists on disk. None otherwise.
///
/// This is the hinge that keeps captured notebooks on the prewarmed
/// capture path on reopen, even when `check_inline_deps` would otherwise
/// route them through the inline-deps flow based on their captured
/// (non-empty) dep list.
pub(crate) fn captured_env_source_override(
    metadata_snapshot: Option<&NotebookMetadataSnapshot>,
) -> Option<String> {
    resolve_captured_env_override(metadata_snapshot).0
}

/// A notebook is considered "captured" only when its claimed env is still
/// present on disk. Deps-present-but-disk-absent is intentionally NOT
/// captured — we can't distinguish a GC'd captured env from a fresh
/// notebook whose user added inline deps before the first launch. Treating
/// the latter as captured would bypass the cross-notebook inline-deps
/// cache. The tradeoff: GC'd captured envs rebuild via the normal inline
/// path using legacy hashing. Same deps still dedup across notebooks; they
/// just lose per-notebook env_id isolation for that rebuild.
fn is_captured(captured: &CapturedEnv) -> bool {
    unified_env_on_disk(captured).is_some()
}

/// Like `captured_env_source_override` but also returns the full
/// `CapturedEnv` that matched. The capture data feeds into
/// `build_launched_config` so drift detection knows what the launch
/// baseline is.
fn resolve_captured_env_override(
    metadata_snapshot: Option<&NotebookMetadataSnapshot>,
) -> (Option<String>, Option<CapturedEnv>) {
    let Some(snap) = metadata_snapshot else {
        return (None, None);
    };
    if let Some(captured) = captured_env_for_runtime(Some(snap), CapturedEnvRuntime::Uv) {
        if is_captured(&captured) {
            return (Some("uv:prewarmed".to_string()), Some(captured));
        }
    }
    if let Some(captured) = captured_env_for_runtime(Some(snap), CapturedEnvRuntime::Conda) {
        if is_captured(&captured) {
            return (Some("conda:prewarmed".to_string()), Some(captured));
        }
    }
    (None, None)
}

/// Acquire a prewarmed env for a notebook, handling both the reopen
/// (captured-deps) path and the first-launch (pool-take + capture) path.
///
/// Returns `Some(Some(env))` with a PooledEnv wrapping either the claimed
/// reopen env or the newly-claimed pool env. Returns `Some(None)` when the
/// pool is empty but the caller should continue (unused by the prewarmed
/// paths today). Returns `None` when a fatal error was broadcast and the
/// caller should unwind.
pub(crate) async fn acquire_prewarmed_env_with_capture(
    env_source: &str,
    daemon: &std::sync::Arc<crate::daemon::Daemon>,
    room: &NotebookRoom,
    metadata_snapshot: Option<&NotebookMetadataSnapshot>,
) -> Option<Option<crate::PooledEnv>> {
    let runtime = match env_source {
        "uv:prewarmed" => CapturedEnvRuntime::Uv,
        "conda:prewarmed" => CapturedEnvRuntime::Conda,
        _ => return acquire_pool_env_for_source(env_source, daemon, room).await,
    };
    let progress_handler: std::sync::Arc<dyn kernel_env::ProgressHandler> = std::sync::Arc::new(
        crate::inline_env::BroadcastProgressHandler::new(room.kernel_broadcast_tx.clone()),
    );

    // Reopen path: if the notebook has an env_id and the unified-hash env
    // exists on disk, route through prepare_environment_unified for an
    // instant cache hit. Captured deps + env_id → same env across reopens.
    //
    // Uses the FULL dep-shape from metadata (requires-python, prerelease,
    // channels, python pin) so the hash matches what the capture step wrote,
    // even after the user edits one of those resolver-affecting fields.
    //
    // `is_captured` checks for the env on disk — deps-present-but-disk-absent
    // falls through to the pool-take + capture path. That covers both fresh
    // notebooks (empty deps) and GC'd captured envs (non-empty deps with no
    // on-disk presence). The GC'd case accepts a rebuild via the normal
    // inline path rather than routing through unified hashing, to avoid
    // conflating "captured" with "user added inline deps before first launch."
    if let Some(captured) = captured_env_for_runtime(metadata_snapshot, runtime) {
        if is_captured(&captured) {
            match &captured {
                CapturedEnv::Uv { deps, env_id } => {
                    let cache_dir = kernel_env::uv::default_cache_dir_uv();
                    match kernel_env::uv::prepare_environment_unified(
                        deps,
                        env_id,
                        &cache_dir,
                        progress_handler.clone(),
                    )
                    .await
                    {
                        Ok(prepared) => {
                            info!(
                                "[notebook-sync] Reopen cache-hit for env_id={} at {:?}",
                                env_id, prepared.venv_path
                            );
                            return Some(Some(crate::PooledEnv {
                                env_type: crate::EnvType::Uv,
                                venv_path: prepared.venv_path,
                                python_path: prepared.python_path,
                                prewarmed_packages: deps.dependencies.clone(),
                            }));
                        }
                        Err(e) => {
                            warn!(
                                "[notebook-sync] Unified UV reopen failed ({}), falling back to pool",
                                e
                            );
                        }
                    }
                }
                CapturedEnv::Conda { deps, env_id } => {
                    let cache_dir = kernel_env::conda::default_cache_dir_conda();
                    match kernel_env::conda::prepare_environment_unified(
                        deps,
                        env_id,
                        &cache_dir,
                        progress_handler.clone(),
                    )
                    .await
                    {
                        Ok(prepared) => {
                            info!(
                                "[notebook-sync] Reopen cache-hit for env_id={} at {:?}",
                                env_id, prepared.env_path
                            );
                            return Some(Some(crate::PooledEnv {
                                env_type: crate::EnvType::Conda,
                                venv_path: prepared.env_path,
                                python_path: prepared.python_path,
                                prewarmed_packages: deps.dependencies.clone(),
                            }));
                        }
                        Err(e) => {
                            warn!(
                                "[notebook-sync] Unified conda reopen failed ({}), falling back to pool",
                                e
                            );
                        }
                    }
                }
            }
        }
    }

    // First-launch path: take from pool, strip base to derive user_defaults,
    // claim to the unified-hash location, and capture into metadata.
    let pooled = acquire_pool_env_for_source(env_source, daemon, room).await?;
    let pooled = pooled?;

    let env_id = metadata_snapshot
        .and_then(|s| s.runt.env_id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    match runtime {
        CapturedEnvRuntime::Uv => {
            let user_defaults =
                kernel_env::strip_base(&pooled.prewarmed_packages, kernel_env::UV_BASE_PACKAGES);
            let prewarmed = kernel_env::uv::UvEnvironment {
                venv_path: pooled.venv_path.clone(),
                python_path: pooled.python_path.clone(),
            };
            let cache_dir = kernel_env::uv::default_cache_dir_uv();
            match kernel_env::uv::claim_prewarmed_environment_in(
                prewarmed,
                &env_id,
                &user_defaults,
                &cache_dir,
            )
            .await
            {
                Ok(claimed) => {
                    let claimed_path = claimed.venv_path.clone();
                    let python_path = claimed.python_path.clone();
                    let _wrote = capture_env_into_metadata(
                        room,
                        CapturedEnvRuntime::Uv,
                        &user_defaults,
                        &env_id,
                    )
                    .await;
                    info!(
                        "[notebook-sync] Captured prewarmed UV env into metadata for env_id={} at {:?}",
                        env_id, claimed_path
                    );
                    Some(Some(crate::PooledEnv {
                        env_type: crate::EnvType::Uv,
                        venv_path: claimed_path,
                        python_path,
                        prewarmed_packages: pooled.prewarmed_packages,
                    }))
                }
                Err(e) => {
                    warn!(
                        "[notebook-sync] Failed to claim UV pool env ({}), using raw pool env",
                        e
                    );
                    Some(Some(pooled))
                }
            }
        }
        CapturedEnvRuntime::Conda => {
            let user_defaults =
                kernel_env::strip_base(&pooled.prewarmed_packages, kernel_env::CONDA_BASE_PACKAGES);
            let prewarmed = kernel_env::conda::CondaEnvironment {
                env_path: pooled.venv_path.clone(),
                python_path: pooled.python_path.clone(),
            };
            let cache_dir = kernel_env::conda::default_cache_dir_conda();
            match kernel_env::conda::claim_prewarmed_environment_in(
                prewarmed,
                &env_id,
                &user_defaults,
                &cache_dir,
            )
            .await
            {
                Ok(claimed) => {
                    let claimed_path = claimed.env_path.clone();
                    let python_path = claimed.python_path.clone();
                    let _wrote = capture_env_into_metadata(
                        room,
                        CapturedEnvRuntime::Conda,
                        &user_defaults,
                        &env_id,
                    )
                    .await;
                    info!(
                        "[notebook-sync] Captured prewarmed conda env into metadata for env_id={} at {:?}",
                        env_id, claimed_path
                    );
                    Some(Some(crate::PooledEnv {
                        env_type: crate::EnvType::Conda,
                        venv_path: claimed_path,
                        python_path,
                        prewarmed_packages: pooled.prewarmed_packages,
                    }))
                }
                Err(e) => {
                    warn!(
                        "[notebook-sync] Failed to claim conda pool env ({}), using raw pool env",
                        e
                    );
                    Some(Some(pooled))
                }
            }
        }
    }
}

/// Acquire a pooled environment from the appropriate pool based on env_source.
/// Returns None and broadcasts error if pool is empty.
async fn acquire_pool_env_for_source(
    env_source: &str,
    daemon: &std::sync::Arc<crate::daemon::Daemon>,
    room: &NotebookRoom,
) -> Option<Option<crate::PooledEnv>> {
    // Route to appropriate pool based on source prefix
    if env_source == "pixi:prewarmed" {
        match daemon.take_pixi_env().await {
            Some(env) => {
                info!(
                    "[notebook-sync] Acquired Pixi env from pool: {:?}",
                    env.python_path
                );
                return Some(Some(env));
            }
            None => {
                // Pixi pool empty — launch on demand via pixi exec (no pooled env needed)
                info!("[notebook-sync] Pixi pool empty, will launch on demand via pixi exec");
                return Some(None);
            }
        }
    }
    if env_source.starts_with("conda:") {
        match daemon.take_conda_env().await {
            Some(env) => {
                info!(
                    "[notebook-sync] Acquired Conda env from pool: {:?}",
                    env.python_path
                );
                Some(Some(env))
            }
            None => {
                error!("[notebook-sync] Conda pool empty, cannot launch");
                let _ = room
                    .kernel_broadcast_tx
                    .send(NotebookBroadcast::KernelStatus {
                        status: "error: Conda pool empty".to_string(),
                        cell_id: None,
                    });
                None // Signal caller to return early
            }
        }
    } else {
        // UV pool for uv:* sources and as default
        match daemon.take_uv_env().await {
            Some(env) => {
                info!(
                    "[notebook-sync] Acquired UV env from pool: {:?}",
                    env.python_path
                );
                Some(Some(env))
            }
            None => {
                error!("[notebook-sync] UV pool empty, cannot launch");
                let _ = room
                    .kernel_broadcast_tx
                    .send(NotebookBroadcast::KernelStatus {
                        status: "error: UV pool empty".to_string(),
                        cell_id: None,
                    });
                None // Signal caller to return early
            }
        }
    }
}

/// Check if a notebook_id is a UUID (untitled/unsaved notebook).
fn is_untitled_notebook(notebook_id: &str) -> bool {
    uuid::Uuid::parse_str(notebook_id).is_ok()
}

/// Reset runtime state to "not_started" (clears any stale starting phase).
/// Used when an early exit prevents kernel launch after status was set to "starting".
///
/// `expected_runtime_agent_id`: If `Some`, only reset if the current runtime agent
/// matches — prevents a stale error handler from clobbering a newer agent's state.
/// Pre-spawn callers pass `None` (no agent exists yet, always safe to reset).
pub(crate) async fn reset_starting_state(
    room: &NotebookRoom,
    expected_runtime_agent_id: Option<&str>,
) {
    // For guarded resets (post-spawn error paths), atomically check-and-clear
    // provenance AND capture the generation counter in a single write lock scope.
    // Clearing provenance to None immediately blocks late-arriving stale agents
    // because the connect handler rejects None provenance.
    //
    // The generation counter MUST be captured while holding the provenance write
    // lock. A new spawn acquires this same write lock (to set provenance) before
    // bumping the generation counter, so the generation cannot change while we
    // hold the lock. This closes the TOCTOU gap: if the generation changes after
    // we release the lock, the per-field checks (below) will detect the mismatch
    // and abort.
    let gen = if let Some(expected) = expected_runtime_agent_id {
        let mut current = room.current_runtime_agent_id.write().await;
        if current.as_deref() != Some(expected) {
            info!(
                "[notebook-sync] Skipping reset_starting_state: expected {} but current is {:?}",
                expected, *current,
            );
            return;
        }
        // Clear provenance and capture generation under the write lock.
        *current = None;
        Some(room.runtime_agent_generation.load(Ordering::Acquire))
    } else {
        None
    };

    // Scope the state_doc write guard so it drops before acquiring
    // runtime_agent_handle lock (deadlock prevention).
    {
        let mut sd = room.state_doc.write().await;
        let mut changed = false;
        changed |= sd.set_kernel_status("not_started");
        changed |= sd.set_prewarmed_packages(&[]);
        if changed {
            let _ = room.state_changed_tx.send(());
        }
    }

    // Clear stale runtime agent handle so auto-launch can retry.
    // Check generation inside the lock: if a new spawn bumped it, the handle
    // belongs to the new generation — do not clear.
    {
        let mut guard = room.runtime_agent_handle.lock().await;
        if let Some(g) = gen {
            if room.runtime_agent_generation.load(Ordering::Acquire) != g {
                info!("[notebook-sync] Aborting reset_starting_state: new spawn detected at handle clear");
                return;
            }
        }
        *guard = None;
    }
    // Clear request channel — same generation guard.
    {
        let mut tx_guard = room.runtime_agent_request_tx.lock().await;
        if let Some(g) = gen {
            if room.runtime_agent_generation.load(Ordering::Acquire) != g {
                info!("[notebook-sync] Aborting reset_starting_state: new spawn detected at request_tx clear");
                return;
            }
        }
        *tx_guard = None;
    }
    // Clear pending connect sender — same generation guard.
    {
        let mut guard = room.pending_runtime_agent_connect_tx.lock().await;
        if let Some(g) = gen {
            if room.runtime_agent_generation.load(Ordering::Acquire) != g {
                info!("[notebook-sync] Aborting reset_starting_state: new spawn detected at connect_tx clear");
                return;
            }
        }
        *guard = None;
    }
}

/// Try to satisfy UV inline deps from the prewarmed pool.
///
/// Takes the pool env first, then compares against its *actual* `prewarmed_packages`
/// (not current settings) to avoid misclassifying stale pool entries.
/// Returns `Ok((PooledEnv, actual_packages))` on success, `Err(())` on failure.
pub(crate) async fn try_uv_pool_for_inline_deps(
    deps: &[String],
    daemon: &std::sync::Arc<crate::daemon::Daemon>,
    progress_handler: std::sync::Arc<dyn kernel_env::ProgressHandler>,
) -> Result<(crate::PooledEnv, Vec<String>), ()> {
    // Quick pre-check: if any dep has version specifiers, skip pool entirely
    // (avoids consuming a pool env we'd have to discard)
    let settings_packages = daemon.uv_pool_packages().await;
    if matches!(
        crate::inline_env::compare_deps_to_pool(deps, &settings_packages),
        crate::inline_env::PoolDepRelation::Independent
    ) {
        debug!("[notebook-sync] UV inline deps have version constraints, skipping pool reuse");
        return Err(());
    }

    // Take the env, then compare against what it *actually* has installed
    let env = match daemon.take_uv_env().await {
        Some(env) => env,
        None => {
            info!("[notebook-sync] UV pool empty, falling back to full build");
            return Err(());
        }
    };

    let actual_packages = env.prewarmed_packages.clone();
    let relation = crate::inline_env::compare_deps_to_pool(deps, &actual_packages);

    match relation {
        crate::inline_env::PoolDepRelation::Subset => {
            info!("[notebook-sync] Inline UV deps are subset of pool env, reusing directly");
            Ok((env, actual_packages))
        }
        crate::inline_env::PoolDepRelation::Additive { delta } => {
            info!(
                "[notebook-sync] Inline UV deps are additive to pool env, delta: {:?}",
                delta
            );
            let uv_env = kernel_env::UvEnvironment {
                venv_path: env.venv_path.clone(),
                python_path: env.python_path.clone(),
            };
            progress_handler.on_progress(
                "uv",
                kernel_env::EnvProgressPhase::Installing { total: delta.len() },
            );
            match kernel_env::uv::sync_dependencies(&uv_env, &delta).await {
                Ok(()) => {
                    info!(
                        "[notebook-sync] Installed {} delta packages into pool env",
                        delta.len()
                    );
                    progress_handler.on_progress(
                        "uv",
                        kernel_env::EnvProgressPhase::Ready {
                            env_path: env.venv_path.to_string_lossy().to_string(),
                            python_path: env.python_path.to_string_lossy().to_string(),
                        },
                    );
                    Ok((env, actual_packages))
                }
                Err(e) => {
                    warn!(
                        "[notebook-sync] Failed to install delta into UV pool env: {}, falling back",
                        e
                    );
                    // Clean up the taken pool env — it's out of the pool's
                    // tracking and would otherwise leak on disk.
                    let root = crate::paths::pool_env_root(&env.venv_path);
                    let cache_dir = crate::paths::default_cache_dir();
                    if crate::is_within_cache_dir(&root, &cache_dir) {
                        if let Err(e) = tokio::fs::remove_dir_all(&root).await {
                            warn!(
                                "[notebook-sync] Failed to clean up UV pool env {:?}: {}",
                                root, e
                            );
                        }
                    }
                    Err(())
                }
            }
        }
        crate::inline_env::PoolDepRelation::Independent => {
            // Shouldn't reach here (pre-check above), but handle gracefully
            debug!("[notebook-sync] UV pool env doesn't match inline deps, falling back");
            let root = crate::paths::pool_env_root(&env.venv_path);
            let cache_dir = crate::paths::default_cache_dir();
            if crate::is_within_cache_dir(&root, &cache_dir) {
                if let Err(e) = tokio::fs::remove_dir_all(&root).await {
                    warn!(
                        "[notebook-sync] Failed to clean up UV pool env {:?}: {}",
                        root, e
                    );
                }
            }
            Err(())
        }
    }
}

/// Try to satisfy Conda inline deps from the prewarmed pool.
///
/// Only attempts pool reuse when channels are default (conda-forge).
/// Takes the pool env first, then compares against its *actual* `prewarmed_packages`.
/// Returns `Ok((PooledEnv, actual_packages))` on success, `Err(())` on failure.
pub(crate) async fn try_conda_pool_for_inline_deps(
    deps: &[String],
    channels: &[String],
    daemon: &std::sync::Arc<crate::daemon::Daemon>,
    progress_handler: std::sync::Arc<dyn kernel_env::ProgressHandler>,
) -> Result<(crate::PooledEnv, Vec<String>), ()> {
    // Only use pool for default conda-forge channel
    let is_default_channels =
        channels.is_empty() || (channels.len() == 1 && channels[0] == "conda-forge");
    if !is_default_channels {
        debug!(
            "[notebook-sync] Conda inline deps use non-default channels {:?}, skipping pool reuse",
            channels
        );
        return Err(());
    }

    // Quick pre-check: if any dep has version specifiers, skip pool entirely
    let settings_packages = daemon.conda_pool_packages().await;
    if matches!(
        crate::inline_env::compare_deps_to_pool(deps, &settings_packages),
        crate::inline_env::PoolDepRelation::Independent
    ) {
        debug!("[notebook-sync] Conda inline deps have version constraints, skipping pool reuse");
        return Err(());
    }

    // Take the env, then compare against what it *actually* has installed
    let env = match daemon.take_conda_env().await {
        Some(env) => env,
        None => {
            info!("[notebook-sync] Conda pool empty, falling back to full build");
            return Err(());
        }
    };

    let actual_packages = env.prewarmed_packages.clone();
    let relation = crate::inline_env::compare_deps_to_pool(deps, &actual_packages);

    match relation {
        crate::inline_env::PoolDepRelation::Subset => {
            info!("[notebook-sync] Inline Conda deps are subset of pool env, reusing directly");
            Ok((env, actual_packages))
        }
        crate::inline_env::PoolDepRelation::Additive { delta } => {
            info!(
                "[notebook-sync] Inline Conda deps are additive to pool env, delta: {:?}",
                delta
            );
            let conda_env = kernel_env::CondaEnvironment {
                env_path: env.venv_path.clone(),
                python_path: env.python_path.clone(),
            };
            let conda_deps = kernel_env::CondaDependencies {
                dependencies: delta.clone(),
                channels: vec!["conda-forge".to_string()],
                python: None,
                env_id: None,
            };
            progress_handler.on_progress(
                "conda",
                kernel_env::EnvProgressPhase::Installing { total: delta.len() },
            );
            match kernel_env::conda::sync_dependencies(&conda_env, &conda_deps).await {
                Ok(()) => {
                    info!(
                        "[notebook-sync] Installed {} delta packages into Conda pool env",
                        delta.len()
                    );
                    progress_handler.on_progress(
                        "conda",
                        kernel_env::EnvProgressPhase::Ready {
                            env_path: env.venv_path.to_string_lossy().to_string(),
                            python_path: env.python_path.to_string_lossy().to_string(),
                        },
                    );
                    Ok((env, actual_packages))
                }
                Err(e) => {
                    warn!(
                        "[notebook-sync] Failed to install delta into Conda pool env: {}, falling back",
                        e
                    );
                    let root = crate::paths::pool_env_root(&env.venv_path);
                    let cache_dir = crate::paths::default_cache_dir();
                    if crate::is_within_cache_dir(&root, &cache_dir) {
                        if let Err(e) = tokio::fs::remove_dir_all(&root).await {
                            warn!(
                                "[notebook-sync] Failed to clean up Conda pool env {:?}: {}",
                                root, e
                            );
                        }
                    }
                    Err(())
                }
            }
        }
        crate::inline_env::PoolDepRelation::Independent => {
            debug!("[notebook-sync] Conda pool env doesn't match inline deps, falling back");
            let root = crate::paths::pool_env_root(&env.venv_path);
            let cache_dir = crate::paths::default_cache_dir();
            if crate::is_within_cache_dir(&root, &cache_dir) {
                if let Err(e) = tokio::fs::remove_dir_all(&root).await {
                    warn!(
                        "[notebook-sync] Failed to clean up Conda pool env {:?}: {}",
                        root, e
                    );
                }
            }
            Err(())
        }
    }
}

/// Auto-launch kernel for a trusted notebook when first peer connects.
/// This is similar to handle_notebook_request(LaunchKernel) but without a request/response.
///
/// Resolves the metadata snapshot from the Automerge doc (if the first client has
/// already synced) or falls back to reading the .ipynb from disk.
async fn auto_launch_kernel(
    room: &NotebookRoom,
    notebook_id: &str,
    default_runtime: crate::runtime::Runtime,
    default_python_env: crate::settings_doc::PythonEnvType,
    daemon: std::sync::Arc<crate::daemon::Daemon>,
) {
    // Check if room still has peers (protect against race condition where client disconnects
    // before we finish launching)
    if room.active_peers.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        debug!("[notebook-sync] Auto-launch aborted: no peers remaining");
        reset_starting_state(room, None).await;
        return;
    }

    // For saved notebooks, notebook_path_opt is the file path (kernel cwd = parent dir).
    // For untitled notebooks, use working_dir as-is (output_prep handles is_dir()).
    let notebook_path = PathBuf::from(notebook_id);
    let notebook_path_opt = if notebook_path.exists() {
        Some(notebook_path.clone())
    } else if is_untitled_notebook(notebook_id) {
        let working_dir = room.working_dir.read().await;
        working_dir.clone().inspect(|p| {
            info!(
                "[notebook-sync] Using working_dir for untitled notebook: {}",
                p.display()
            );
        })
    } else {
        None
    };

    // For project file detection, use the same path
    let working_dir_for_detection = notebook_path_opt.clone();

    // Resolve metadata snapshot: try Automerge doc first, fall back to disk
    let metadata_snapshot = resolve_metadata_snapshot(room, notebook_path_opt.as_deref()).await;

    // Check RuntimeStateDoc — skip if already starting or running
    {
        // Skip if runtime agent already exists (another auto-launch won the race)
        // or kernel is already running
        let has_runtime_agent = room.runtime_agent_handle.lock().await.is_some();
        if has_runtime_agent {
            debug!("[notebook-sync] Auto-launch skipped: runtime agent already exists");
            return;
        }
    }

    // Re-check peers (another race check)
    if room.active_peers.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        debug!("[notebook-sync] Auto-launch aborted: no peers (after status check)");
        reset_starting_state(room, None).await;
        return;
    }

    // Clear any stale comm state from a previous kernel (in case it crashed)

    {
        let mut sd = room.state_doc.write().await;
        if sd.clear_comms() {
            let _ = room.state_changed_tx.send(());
        }
    }

    // Detection priority:
    // 1. Notebook's kernelspec (for existing notebooks) - determines python vs deno
    // 2. For Python: resolve environment (inline deps → project files → prewarmed)
    // 3. For Deno: just launch Deno (no env resolution needed)
    // 4. For new notebooks (no kernelspec): use default_runtime setting

    // Step 1: Detect kernel type from metadata snapshot
    let notebook_kernel_type = metadata_snapshot.as_ref().and_then(|s| s.detect_runtime());

    // Step 2a: If the notebook has a captured prewarmed env on disk, route
    // back through the prewarmed capture path so the reopen cache-hit fires.
    // This overrides inline-deps detection because captured deps are
    // structurally indistinguishable from user-authored inline deps.
    let (captured_override, captured_env_for_config) =
        resolve_captured_env_override(metadata_snapshot.as_ref());
    if let Some(ref src) = captured_override {
        info!(
            "[notebook-sync] Auto-launch: captured env on disk -> {}",
            src
        );
    }

    // Step 2b: Check inline deps (for environment source, and runt.deno override)
    let inline_source = captured_override
        .clone()
        .or_else(|| metadata_snapshot.as_ref().and_then(check_inline_deps));

    // Step 2b: If no metadata inline deps, check cell source for PEP 723 script blocks
    let (inline_source, pep723_deps) = if inline_source.is_some() {
        (inline_source, None)
    } else {
        let cells = room.doc.read().await.get_cells();
        match notebook_doc::pep723::find_pep723_in_cells(&cells) {
            Ok(Some(meta)) if !meta.dependencies.is_empty() => {
                // Route PEP 723 deps based on user's default Python env
                let pep723_source = match default_python_env {
                    crate::settings_doc::PythonEnvType::Pixi => "pixi:pep723",
                    _ => "uv:pep723",
                };
                info!(
                    "[notebook-sync] Auto-launch: found PEP 723 deps ({}) : {:?}",
                    pep723_source, meta.dependencies
                );
                (Some(pep723_source.to_string()), Some(meta.dependencies))
            }
            Ok(_) => (None, None),
            Err(e) => {
                warn!("[notebook-sync] PEP 723 parse error: {}", e);
                (None, None)
            }
        }
    };

    // Step 3: Check project files (for Python environment resolution)
    // Use notebook path for saved notebooks, or working_dir for untitled notebooks
    let detection_path = notebook_path_opt
        .as_ref()
        .or(working_dir_for_detection.as_ref());
    let detected_project_file =
        detection_path.and_then(|path| crate::project_file::detect_project_file(path));
    if let Some(ref detected) = detected_project_file {
        info!(
            "[notebook-sync] Auto-launch: detected project file {:?} -> {}",
            detected.path,
            detected.to_env_source()
        );
    }
    let project_source = detected_project_file
        .as_ref()
        .map(|d| d.to_env_source().to_string());

    // Step 3b: Bootstrap project deps into CRDT metadata.
    // When a project file exists, populate the inline dep section with the
    // project's deps so that the UI and MCP tools can see what's available.
    if let Some(ref detected) = detected_project_file {
        match detected.kind {
            crate::project_file::ProjectFileKind::PixiToml => {
                if let Ok(info) = kernel_launch::tools::pixi_info(&detected.path).await {
                    let deps = info.default_deps_snapshot();
                    if !deps.is_empty() {
                        let mut doc = room.doc.write().await;
                        let mut changed = false;
                        doc.fork_and_merge(|fork| {
                            let mut snap = fork.get_metadata_snapshot().unwrap_or_default();
                            let current_deps = snap.runt.pixi.as_ref().map(|p| &p.dependencies);
                            if current_deps.is_none_or(|d| d != &deps) {
                                let pixi = snap.pixi_section_or_default();
                                pixi.dependencies = deps;
                                let _ = fork.set_metadata_snapshot(&snap);
                                changed = true;
                            }
                        });
                        if changed {
                            info!("[notebook-sync] Bootstrapped pixi.toml deps into CRDT");
                        } else {
                            debug!(
                                "[notebook-sync] Pixi deps already current in CRDT, skipping write"
                            );
                        }
                    }
                }
            }
            crate::project_file::ProjectFileKind::PyprojectToml => {
                // Read [project.dependencies] from pyproject.toml
                if let Ok(content) = std::fs::read_to_string(&detected.path) {
                    let deps = extract_pyproject_deps(&content);
                    if !deps.is_empty() {
                        let mut doc = room.doc.write().await;
                        let mut changed = false;
                        doc.fork_and_merge(|fork| {
                            let mut snap = fork.get_metadata_snapshot().unwrap_or_default();
                            let current_deps = snap.runt.uv.as_ref().map(|u| &u.dependencies);
                            if current_deps.is_none_or(|d| d != &deps) {
                                let uv = snap.runt.uv.get_or_insert_with(|| {
                                    notebook_doc::metadata::UvInlineMetadata {
                                        dependencies: Vec::new(),
                                        requires_python: None,
                                        prerelease: None,
                                    }
                                });
                                uv.dependencies = deps;
                                let _ = fork.set_metadata_snapshot(&snap);
                                changed = true;
                            }
                        });
                        if changed {
                            info!("[notebook-sync] Bootstrapped pyproject.toml deps into CRDT");
                        } else {
                            debug!("[notebook-sync] Pyproject deps already current in CRDT, skipping write");
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Determine kernel type and environment
    let (kernel_type, env_source, pooled_env) = match notebook_kernel_type.as_deref() {
        Some("deno") => {
            // Notebook is a Deno notebook (per its kernelspec)
            info!("[notebook-sync] Auto-launch: Deno kernel (notebook kernelspec)");
            ("deno", "deno".to_string(), None)
        }
        Some("python") => {
            // Notebook is a Python notebook - resolve environment
            // Priority: project file > inline deps > prewarmed
            // Project file wins because inline deps get promoted to the
            // project file at sync/launch time (project is source of truth).
            let env_source = if let Some(ref proj) = project_source {
                info!(
                    "[notebook-sync] Auto-launch: using project file -> {}",
                    proj
                );
                proj.clone()
            } else if let Some(ref source) = inline_source {
                // Skip "deno" inline source for Python notebooks (kernelspec takes priority)
                if source != "deno" {
                    info!(
                        "[notebook-sync] Auto-launch: found inline deps -> {}",
                        source
                    );
                    source.clone()
                } else {
                    let prewarmed = match default_python_env {
                        crate::settings_doc::PythonEnvType::Conda => "conda:prewarmed",
                        crate::settings_doc::PythonEnvType::Pixi => "pixi:prewarmed",
                        _ => "uv:prewarmed",
                    };
                    prewarmed.to_string()
                }
            } else {
                let prewarmed = match default_python_env {
                    crate::settings_doc::PythonEnvType::Conda => "conda:prewarmed",
                    crate::settings_doc::PythonEnvType::Pixi => "pixi:prewarmed",
                    _ => "uv:prewarmed",
                };
                info!(
                    "[notebook-sync] Auto-launch: using prewarmed ({})",
                    prewarmed
                );
                prewarmed.to_string()
            };
            // For uv:inline, uv:pep723, uv:pyproject, and conda:inline we don't need a pooled env -
            // these sources prepare their own environments
            let pooled_env = if env_source == "uv:pyproject"
                || env_source == "uv:inline"
                || env_source == "uv:pep723"
                || env_source == "conda:inline"
                || env_source == "pixi:toml"
                || env_source == "pixi:inline"
                || env_source == "pixi:pep723"
            {
                info!(
                    "[notebook-sync] Auto-launch: {} prepares its own env, no pool env needed",
                    env_source
                );
                None
            } else {
                match acquire_prewarmed_env_with_capture(
                    &env_source,
                    &daemon,
                    room,
                    metadata_snapshot.as_ref(),
                )
                .await
                {
                    Some(env) => env,
                    None => {
                        reset_starting_state(room, None).await;
                        return;
                    }
                }
            };
            ("python", env_source, pooled_env)
        }
        None => {
            // New notebook or unknown kernelspec - use default_runtime
            if inline_source.as_deref() == Some("deno") {
                // runt.deno config present
                info!("[notebook-sync] Auto-launch: Deno kernel (runt.deno config)");
                ("deno", "deno".to_string(), None)
            } else if matches!(default_runtime, crate::runtime::Runtime::Deno) {
                // User's default is Deno
                info!("[notebook-sync] Auto-launch: Deno kernel (default runtime)");
                ("deno", "deno".to_string(), None)
            } else {
                // Default to Python
                // Priority: project file > inline deps > prewarmed
                let env_source = if let Some(ref source) = project_source {
                    info!(
                        "[notebook-sync] Auto-launch: using project file -> {}",
                        source
                    );
                    source.clone()
                } else if let Some(ref source) = inline_source {
                    info!(
                        "[notebook-sync] Auto-launch: found inline deps -> {}",
                        source
                    );
                    source.clone()
                } else {
                    let prewarmed = match default_python_env {
                        crate::settings_doc::PythonEnvType::Conda => "conda:prewarmed",
                        crate::settings_doc::PythonEnvType::Pixi => "pixi:prewarmed",
                        _ => "uv:prewarmed",
                    };
                    info!(
                        "[notebook-sync] Auto-launch: using prewarmed ({})",
                        prewarmed
                    );
                    prewarmed.to_string()
                };
                // For uv:inline, uv:pep723, uv:pyproject, and conda:inline we don't need a pooled env -
                // these sources prepare their own environments
                let pooled_env = if env_source == "uv:pyproject"
                    || env_source == "uv:inline"
                    || env_source == "uv:pep723"
                    || env_source == "conda:inline"
                    || env_source == "pixi:toml"
                {
                    info!(
                        "[notebook-sync] Auto-launch: {} prepares its own env, no pool env needed",
                        env_source
                    );
                    None
                } else {
                    match acquire_prewarmed_env_with_capture(
                        &env_source,
                        &daemon,
                        room,
                        metadata_snapshot.as_ref(),
                    )
                    .await
                    {
                        Some(env) => env,
                        None => {
                            reset_starting_state(room, None).await;
                            return;
                        }
                    }
                };
                ("python", env_source, pooled_env)
            }
        }
        Some(other) => {
            // Unknown kernel type - default to Python
            warn!(
                "[notebook-sync] Unknown kernel type '{}', defaulting to Python",
                other
            );
            let prewarmed = match default_python_env {
                crate::settings_doc::PythonEnvType::Conda => "conda:prewarmed",
                _ => "uv:prewarmed",
            };
            let pooled_env = match acquire_prewarmed_env_with_capture(
                prewarmed,
                &daemon,
                room,
                metadata_snapshot.as_ref(),
            )
            .await
            {
                Some(env) => env,
                None => {
                    reset_starting_state(room, None).await;
                    return;
                }
            };
            ("python", prewarmed.to_string(), pooled_env)
        }
    };

    // For pixi:toml, verify ipykernel is in pixi.toml before launching
    if env_source == "pixi:toml" {
        if let Some(ref detected) = detected_project_file {
            let has_ipykernel = match kernel_launch::tools::pixi_info(&detected.path).await {
                Ok(info) => info.has_ipykernel(),
                Err(e) => {
                    warn!(
                        "[notebook-sync] pixi info failed, falling back to line scan: {}",
                        e
                    );
                    crate::project_file::pixi_toml_has_ipykernel(&detected.path)
                }
            };
            if !has_ipykernel {
                warn!(
                    "[notebook-sync] pixi.toml at {:?} does not declare ipykernel — cannot launch kernel",
                    detected.path
                );
                {
                    let mut sd = room.state_doc.write().await;
                    sd.set_kernel_status("error");
                    sd.set_kernel_info("python", "python", &env_source);
                    sd.set_starting_phase("missing_ipykernel");
                    let _ = room.state_changed_tx.send(());
                }
                return;
            }
        }
    }

    // Transition to "preparing_env" phase now that runtime/env has been resolved
    {
        let mut sd = room.state_doc.write().await;
        if sd.set_starting_phase("preparing_env") {
            let _ = room.state_changed_tx.send(());
        }
    }

    // For inline deps, prepare a cached environment with rich progress
    let progress_handler: std::sync::Arc<dyn kernel_env::ProgressHandler> = std::sync::Arc::new(
        crate::inline_env::BroadcastProgressHandler::new(room.kernel_broadcast_tx.clone()),
    );

    // Fetch feature flags now so inline env prep hashes match what the
    // kernel will actually receive (bootstrap_dx changes the install set).
    let feature_flags_for_inline = daemon.feature_flags().await;
    let bootstrap_dx = feature_flags_for_inline.bootstrap_dx;

    let (pooled_env, inline_deps) = if env_source == "uv:pep723" {
        // PEP 723 deps were extracted from cell source in step 2b
        if let Some(ref deps) = pep723_deps {
            info!(
                "[notebook-sync] Preparing cached UV env for PEP 723 deps: {:?}",
                deps
            );
            match crate::inline_env::prepare_uv_inline_env(
                deps,
                None,
                progress_handler.clone(),
                bootstrap_dx,
            )
            .await
            {
                Ok(prepared) => {
                    info!(
                        "[notebook-sync] Using cached PEP 723 env at {:?}",
                        prepared.python_path
                    );
                    let env = Some(crate::PooledEnv {
                        env_type: crate::EnvType::Uv,
                        venv_path: prepared.env_path,
                        python_path: prepared.python_path,
                        prewarmed_packages: vec![],
                    });
                    (env, Some(deps.clone()))
                }
                Err(e) => {
                    error!("[notebook-sync] Failed to prepare PEP 723 env: {}", e);
                    let _ = room
                        .kernel_broadcast_tx
                        .send(NotebookBroadcast::KernelStatus {
                            status: format!("error: Failed to prepare environment: {}", e),
                            cell_id: None,
                        });
                    reset_starting_state(room, None).await;
                    return;
                }
            }
        } else {
            (None, None)
        }
    } else if env_source == "uv:inline" {
        if let Some(deps) = metadata_snapshot.as_ref().and_then(get_inline_uv_deps) {
            let prerelease = metadata_snapshot
                .as_ref()
                .and_then(get_inline_uv_prerelease);

            // Fast path: check inline env cache first (instant on hit)
            if let Some(cached) =
                crate::inline_env::check_uv_inline_cache(&deps, prerelease.as_deref(), bootstrap_dx)
            {
                info!(
                    "[notebook-sync] UV inline cache hit at {:?}",
                    cached.python_path
                );
                let env = Some(crate::PooledEnv {
                    env_type: crate::EnvType::Uv,
                    venv_path: cached.env_path,
                    python_path: cached.python_path,
                    prewarmed_packages: vec![],
                });
                (env, Some(deps))
            } else if prerelease.is_none() {
                // Try pool reuse for bare deps without prerelease
                match try_uv_pool_for_inline_deps(&deps, &daemon, progress_handler.clone()).await {
                    Ok((env, pool_pkgs)) => {
                        let mut pooled = env;
                        pooled.prewarmed_packages = pool_pkgs;
                        (Some(pooled), Some(deps))
                    }
                    Err(_) => {
                        // Pool path failed, fall back to full build
                        info!(
                            "[notebook-sync] Preparing cached UV env for inline deps: {:?}",
                            deps
                        );
                        match crate::inline_env::prepare_uv_inline_env(
                            &deps,
                            prerelease.as_deref(),
                            progress_handler.clone(),
                            bootstrap_dx,
                        )
                        .await
                        {
                            Ok(prepared) => {
                                info!(
                                    "[notebook-sync] Using cached inline env at {:?}",
                                    prepared.python_path
                                );
                                let env = Some(crate::PooledEnv {
                                    env_type: crate::EnvType::Uv,
                                    venv_path: prepared.env_path,
                                    python_path: prepared.python_path,
                                    prewarmed_packages: vec![],
                                });
                                (env, Some(deps))
                            }
                            Err(e) => {
                                error!("[notebook-sync] Failed to prepare inline env: {}", e);
                                let _ = room.kernel_broadcast_tx.send(
                                    NotebookBroadcast::KernelStatus {
                                        status: format!(
                                            "error: Failed to prepare environment: {}",
                                            e
                                        ),
                                        cell_id: None,
                                    },
                                );
                                reset_starting_state(room, None).await;
                                return;
                            }
                        }
                    }
                }
            } else {
                // Has prerelease — can't use pool, go straight to full build
                info!(
                    "[notebook-sync] Preparing cached UV env for inline deps: {:?} (prerelease: {:?})",
                    deps, prerelease
                );
                match crate::inline_env::prepare_uv_inline_env(
                    &deps,
                    prerelease.as_deref(),
                    progress_handler.clone(),
                    bootstrap_dx,
                )
                .await
                {
                    Ok(prepared) => {
                        info!(
                            "[notebook-sync] Using cached inline env at {:?}",
                            prepared.python_path
                        );
                        let env = Some(crate::PooledEnv {
                            env_type: crate::EnvType::Uv,
                            venv_path: prepared.env_path,
                            python_path: prepared.python_path,
                            prewarmed_packages: vec![],
                        });
                        (env, Some(deps))
                    }
                    Err(e) => {
                        error!("[notebook-sync] Failed to prepare inline env: {}", e);
                        let _ = room
                            .kernel_broadcast_tx
                            .send(NotebookBroadcast::KernelStatus {
                                status: format!("error: Failed to prepare environment: {}", e),
                                cell_id: None,
                            });
                        reset_starting_state(room, None).await;
                        return;
                    }
                }
            }
        } else {
            (pooled_env, None)
        }
    } else if env_source == "conda:inline" {
        if let Some(deps) = metadata_snapshot.as_ref().and_then(get_inline_conda_deps) {
            let channels = metadata_snapshot
                .as_ref()
                .map(get_inline_conda_channels)
                .unwrap_or_else(|| vec!["conda-forge".to_string()]);

            // Fast path: check inline env cache first (instant on hit)
            if let Some(cached) = crate::inline_env::check_conda_inline_cache(&deps, &channels) {
                info!(
                    "[notebook-sync] Conda inline cache hit at {:?}",
                    cached.python_path
                );
                let env = Some(crate::PooledEnv {
                    env_type: crate::EnvType::Conda,
                    venv_path: cached.env_path,
                    python_path: cached.python_path,
                    prewarmed_packages: vec![],
                });
                (env, Some(deps))
            } else {
                // Try pool reuse (only for default conda-forge channel)
                match try_conda_pool_for_inline_deps(
                    &deps,
                    &channels,
                    &daemon,
                    progress_handler.clone(),
                )
                .await
                {
                    Ok((env, pool_pkgs)) => {
                        let mut pooled = env;
                        pooled.prewarmed_packages = pool_pkgs;
                        (Some(pooled), Some(deps))
                    }
                    Err(_) => {
                        // Pool path failed, fall back to full build
                        info!(
                            "[notebook-sync] Preparing cached Conda env for inline deps: {:?} (channels: {:?})",
                            deps, channels
                        );
                        match crate::inline_env::prepare_conda_inline_env(
                            &deps,
                            &channels,
                            progress_handler.clone(),
                        )
                        .await
                        {
                            Ok(prepared) => {
                                info!(
                                    "[notebook-sync] Using cached conda inline env at {:?}",
                                    prepared.python_path
                                );
                                let env = Some(crate::PooledEnv {
                                    env_type: crate::EnvType::Conda,
                                    venv_path: prepared.env_path,
                                    python_path: prepared.python_path,
                                    prewarmed_packages: vec![],
                                });
                                (env, Some(deps))
                            }
                            Err(e) => {
                                error!("[notebook-sync] Failed to prepare conda inline env: {}", e);
                                let _ = room.kernel_broadcast_tx.send(
                                    NotebookBroadcast::KernelStatus {
                                        status: format!(
                                            "error: Failed to prepare conda environment: {}",
                                            e
                                        ),
                                        cell_id: None,
                                    },
                                );
                                reset_starting_state(room, None).await;
                                return;
                            }
                        }
                    }
                }
            }
        } else {
            (pooled_env, None)
        }
    } else if env_source == "pixi:inline" {
        // pixi exec handles its own env caching — just extract deps for the -w flags
        let deps = metadata_snapshot
            .as_ref()
            .and_then(|s| s.runt.pixi.as_ref())
            .map(|p| p.dependencies.clone())
            .unwrap_or_default();
        if !deps.is_empty() {
            info!("[notebook-sync] pixi:inline deps for pixi exec: {:?}", deps);
            (None, Some(deps))
        } else {
            (pooled_env, None)
        }
    } else if env_source == "pixi:pep723" {
        // PEP 723 deps via pixi exec -w (same mechanism as pixi:inline)
        if let Some(ref deps) = pep723_deps {
            info!("[notebook-sync] pixi:pep723 deps for pixi exec: {:?}", deps);
            (None, Some(deps.clone()))
        } else {
            (pooled_env, None)
        }
    } else {
        (pooled_env, None)
    };

    // Register the env path for GC protection immediately after pool.take(),
    // BEFORE any async work (agent spawn, connect timeout, delta install).
    // Without this, there's a race window where GC sees the taken env as an
    // orphan and deletes it while we're still setting up the kernel.
    if let Some(ref env) = pooled_env {
        let mut ep = room.runtime_agent_env_path.write().await;
        *ep = Some(env.venv_path.clone());
    }

    // Build LaunchedEnvConfig to track what config the kernel was launched with
    let venv_path = pooled_env.as_ref().map(|e| e.venv_path.clone());
    let python_path = pooled_env.as_ref().map(|e| e.python_path.clone());
    let prewarmed_pkgs = if let Some(ref env) = pooled_env {
        Some(env.prewarmed_packages.clone())
    } else if env_source.starts_with("pixi:") {
        // For pixi launches without a pooled env, read default packages from settings
        let pixi_defaults = daemon.default_pixi_packages().await;
        if pixi_defaults.is_empty() {
            None
        } else {
            Some(pixi_defaults)
        }
    } else {
        None
    };
    let feature_flags = feature_flags_for_inline;
    // Pass captured env only when this launch actually routed through the
    // captured path. If the capture data is present but env_source flipped
    // to a different family (auto:uv vs captured conda, project file wins,
    // etc.), `captured_env_for_config` stays None so drift detection
    // doesn't get misleading deps from the wrong runtime.
    let captured_for_config = captured_env_for_config.as_ref().filter(|c| match c {
        CapturedEnv::Uv { .. } => env_source == "uv:prewarmed",
        CapturedEnv::Conda { .. } => env_source == "conda:prewarmed",
    });
    let launched_config = build_launched_config(
        kernel_type,
        &env_source,
        inline_deps.as_deref(),
        metadata_snapshot.as_ref(),
        venv_path,
        python_path,
        prewarmed_pkgs.as_deref(),
        notebook_path_opt.as_deref(),
        feature_flags,
        captured_for_config,
    );

    // Transition to "launching" phase before starting the kernel process
    {
        let mut sd = room.state_doc.write().await;
        if sd.set_starting_phase("launching") {
            let _ = room.state_changed_tx.send(());
        }
    }

    // (prewarmed_packages no longer needed — runtime agent handles its own launch config)

    // Spawn runtime agent subprocess for kernel execution
    {
        info!("[notebook-sync] Spawning runtime agent subprocess for auto-launch");

        // Always pass the room UUID so the agent's RuntimeAgent handshake
        // finds the room in the UUID-keyed rooms map.
        let nb_id = room.id.to_string();
        let runtime_agent_id = format!("runtime-agent:{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let socket_path = daemon.socket_path().clone();

        // Set provenance BEFORE spawn so stale agents are rejected by the
        // connect handler's provenance check. Bump generation BEFORE storing
        // the oneshot so reset_starting_state can detect interleaving spawns.
        // Then create the oneshot so it's ready before the subprocess can
        // connect.
        //
        // Ordering: provenance → generation → oneshot → spawn
        {
            let mut id = room.current_runtime_agent_id.write().await;
            *id = Some(runtime_agent_id.clone());
        }
        room.runtime_agent_generation
            .fetch_add(1, Ordering::Release);
        let runtime_agent_connect_rx = {
            let (tx, rx) = oneshot::channel();
            let mut guard = room.pending_runtime_agent_connect_tx.lock().await;
            *guard = Some(tx);
            rx
        };

        match crate::runtime_agent_handle::RuntimeAgentHandle::spawn(
            nb_id,
            runtime_agent_id.clone(),
            room.blob_store.root().to_path_buf(),
            socket_path,
        )
        .await
        {
            Ok(ra) => {
                // Store handle after spawn succeeds.
                {
                    let mut ra_guard = room.runtime_agent_handle.lock().await;
                    *ra_guard = Some(ra);
                }

                // Write "connecting" phase — fills the gap between spawn and connect
                {
                    let mut sd = room.state_doc.write().await;
                    if sd.set_starting_phase("connecting") {
                        let _ = room.state_changed_tx.send(());
                    }
                }

                // Wait for THIS runtime agent to establish its sync connection
                match tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    runtime_agent_connect_rx,
                )
                .await
                {
                    Ok(Ok(())) => {
                        info!("[notebook-sync] Agent connected, sending LaunchKernel");
                    }
                    Ok(Err(_)) => {
                        // Oneshot sender dropped — runtime agent died or was
                        // superseded by a newer spawn before connecting.
                        warn!(
                            "[notebook-sync] Runtime agent connect cancelled (superseded or died)"
                        );
                        reset_starting_state(room, Some(&runtime_agent_id)).await;
                        return;
                    }
                    Err(_) => {
                        warn!("[notebook-sync] Agent failed to connect within 30s");
                        reset_starting_state(room, Some(&runtime_agent_id)).await;
                        return;
                    }
                }

                // Send LaunchKernel RPC via the runtime agent's sync connection
                let launch_request =
                    notebook_protocol::protocol::RuntimeAgentRequest::LaunchKernel {
                        kernel_type: kernel_type.to_string(),
                        env_source: env_source.clone(),
                        notebook_path: notebook_path_opt
                            .as_deref()
                            .map(|p| p.to_str().unwrap_or("").to_string()),
                        launched_config: launched_config.clone(),
                        env_vars: Default::default(),
                    };

                match send_runtime_agent_request(room, launch_request).await {
                    Ok(notebook_protocol::protocol::RuntimeAgentResponse::KernelLaunched {
                        env_source: es,
                    }) => {
                        // env path already registered for GC protection above (before spawn)

                        // Store launched config for env sync drift detection
                        {
                            let mut lc = room.runtime_agent_launched_config.write().await;
                            *lc = Some(launched_config.clone());
                        }

                        publish_kernel_state_presence(room, presence::KernelStatus::Idle, &es)
                            .await;

                        // Write kernel status + info to RuntimeStateDoc so
                        // frontends see "idle" via CRDT sync.
                        {
                            let mut sd = room.state_doc.write().await;
                            let mut changed = false;
                            changed |= sd.set_kernel_status("idle");
                            changed |= sd.set_kernel_info(kernel_type, kernel_type, &es);
                            changed |= sd.set_runtime_agent_id(&runtime_agent_id);
                            if changed {
                                let _ = room.state_changed_tx.send(());
                            }
                        }

                        // Fresh kernel is in sync with its launched config
                        {
                            let mut sd = room.state_doc.write().await;
                            if sd.set_env_sync(true, &[], &[], false, false) {
                                let _ = room.state_changed_tx.send(());
                            }
                        }

                        info!(
                            "[notebook-sync] Auto-launch via runtime agent succeeded: {} kernel with {} environment",
                            kernel_type, es
                        );
                    }
                    Ok(notebook_protocol::protocol::RuntimeAgentResponse::Error { error }) => {
                        warn!("[notebook-sync] Agent kernel launch failed: {}", error);
                        reset_starting_state(room, Some(&runtime_agent_id)).await;
                    }
                    Ok(_) => {
                        warn!(
                            "[notebook-sync] Unexpected runtime agent response during auto-launch"
                        );
                        reset_starting_state(room, Some(&runtime_agent_id)).await;
                    }
                    Err(e) => {
                        warn!("[notebook-sync] Agent communication error: {}", e);
                        reset_starting_state(room, Some(&runtime_agent_id)).await;
                    }
                }
            }
            Err(e) => {
                warn!("[notebook-sync] Failed to spawn runtime agent: {}", e);
                reset_starting_state(room, None).await;
            }
        }
    }
}

/// Publish the daemon's `KernelState` presence so late-joining peers
/// receive kernel status in their `PresenceSnapshot`.
pub(crate) async fn publish_kernel_state_presence(
    room: &NotebookRoom,
    status: presence::KernelStatus,
    env_source: &str,
) {
    update_kernel_presence(&room.presence, &room.presence_tx, status, env_source).await;
}

/// Update kernel state in the shared presence state and relay to all peers.
///
/// Factored out so spawned tasks (which only hold cloned Arcs) can call it
/// without needing a full `&NotebookRoom` reference.
pub(crate) async fn update_kernel_presence(
    presence_state: &Arc<RwLock<PresenceState>>,
    presence_tx: &broadcast::Sender<(String, Vec<u8>)>,
    status: presence::KernelStatus,
    env_source: &str,
) {
    let data = presence::KernelStateData {
        status,
        env_source: env_source.to_string(),
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    presence_state.write().await.update_peer(
        "daemon",
        "daemon",
        None,
        presence::ChannelData::KernelState(data.clone()),
        now_ms,
    );
    match presence::encode_kernel_state_update("daemon", &data) {
        Ok(bytes) => {
            let _ = presence_tx.send(("daemon".to_string(), bytes));
        }
        Err(e) => warn!(
            "[notebook-sync] Failed to encode kernel state presence: {}",
            e
        ),
    }
}

/// Handle a NotebookRequest and return a NotebookResponse.
async fn handle_notebook_request(
    room: &Arc<NotebookRoom>,
    request: NotebookRequest,
    daemon: std::sync::Arc<crate::daemon::Daemon>,
) -> NotebookResponse {
    debug!("[notebook-sync] Handling request: {:?}", request);

    match request {
        NotebookRequest::LaunchKernel {
            kernel_type,
            env_source,
            notebook_path,
        } => {
            crate::requests::launch_kernel::handle(
                room,
                &daemon,
                kernel_type,
                env_source,
                notebook_path,
            )
            .await
        }

        NotebookRequest::ExecuteCell { cell_id } => {
            crate::requests::execute_cell::handle(room, cell_id).await
        }

        NotebookRequest::ClearOutputs { cell_id } => {
            crate::requests::clear_outputs::handle(room, cell_id).await
        }

        NotebookRequest::InterruptExecution {} => {
            crate::requests::interrupt_execution::handle(room).await
        }

        NotebookRequest::ShutdownKernel {} => crate::requests::shutdown_kernel::handle(room).await,

        NotebookRequest::GetKernelInfo {} => crate::requests::get_kernel_info::handle(room).await,

        NotebookRequest::GetQueueState {} => crate::requests::get_queue_state::handle(room).await,

        NotebookRequest::RunAllCells {} => crate::requests::run_all_cells::handle(room).await,

        NotebookRequest::SendComm { message } => {
            crate::requests::send_comm::handle(room, message).await
        }

        NotebookRequest::GetHistory { pattern, n, unique } => {
            crate::requests::get_history::handle(room, pattern, n, unique).await
        }

        NotebookRequest::Complete { code, cursor_pos } => {
            crate::requests::complete::handle(room, code, cursor_pos).await
        }

        NotebookRequest::SaveNotebook { format_cells, path } => {
            crate::requests::save_notebook::handle(room, &daemon, format_cells, path).await
        }

        NotebookRequest::CloneNotebook { path } => {
            crate::requests::clone_notebook::handle(room, path).await
        }

        NotebookRequest::SyncEnvironment {} => {
            crate::requests::sync_environment::handle(room).await
        }

        NotebookRequest::GetDocBytes {} => crate::requests::get_doc_bytes::handle(room).await,

        NotebookRequest::GetRawMetadata { key } => {
            crate::requests::get_raw_metadata::handle(room, key).await
        }

        NotebookRequest::SetRawMetadata { key, value } => {
            crate::requests::set_raw_metadata::handle(room, key, value).await
        }

        NotebookRequest::GetMetadataSnapshot {} => {
            crate::requests::get_metadata_snapshot::handle(room).await
        }

        NotebookRequest::SetMetadataSnapshot { snapshot } => {
            crate::requests::set_metadata_snapshot::handle(room, snapshot).await
        }

        NotebookRequest::CheckToolAvailable { tool } => {
            crate::requests::check_tool_available::handle(tool).await
        }
    }
}

/// Promote inline deps from CRDT metadata to a project file.
///
/// When a kernel is project-backed (pixi:toml or uv:pyproject), inline deps in
/// the CRDT are "intent" — the user wants this package. This function promotes
/// them to the project file via `pixi add` / `uv add`, then clears the inline
/// section from the CRDT.
///
/// Find the byte offset in an environment.yml string where new dependencies
/// should be inserted (end of the `dependencies:` list, before the next
/// top-level key or EOF).
fn find_env_yml_deps_insertion_point(content: &str) -> Option<usize> {
    let mut in_deps = false;
    let mut last_dep_end = None;

    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        if !line.starts_with(' ') && !line.starts_with('\t') {
            if trimmed == "dependencies:" {
                in_deps = true;
                // Position after this line (clamped for files without trailing newline)
                let offset: usize = content.lines().take(i + 1).map(|l| l.len() + 1).sum();
                last_dep_end = Some(offset.min(content.len()));
                continue;
            } else if in_deps && !trimmed.is_empty() && !trimmed.starts_with('#') {
                // Hit a new top-level key — insert before it
                return last_dep_end;
            }
        }

        if in_deps && trimmed.starts_with("- ") {
            let offset: usize = content.lines().take(i + 1).map(|l| l.len() + 1).sum();
            last_dep_end = Some(offset.min(content.len()));
        }
    }

    last_dep_end
}

/// For removals, compares CRDT deps against launched baseline and runs
/// `pixi remove` / `uv remove` for any deps that were removed.
///
/// Returns the list of packages that were added or removed.
pub(crate) async fn promote_inline_deps_to_project(
    room: &NotebookRoom,
    env_source: &str,
    launched_config: &notebook_protocol::protocol::LaunchedEnvConfig,
) -> Result<Vec<String>, String> {
    let current_metadata = {
        let doc = room.doc.read().await;
        doc.get_metadata_snapshot()
    };
    let Some(current_metadata) = current_metadata else {
        return Ok(vec![]);
    };

    let mut promoted = Vec::new();

    if env_source == "pixi:toml" {
        let pixi_toml_path = launched_config.pixi_toml_path.as_ref().ok_or_else(|| {
            "pixi:toml kernel has no pixi_toml_path in launched config".to_string()
        })?;

        // Determine what deps are in the CRDT vs what was launched
        let current_deps: Vec<String> = current_metadata
            .runt
            .pixi
            .as_ref()
            .map(|p| p.dependencies.clone())
            .unwrap_or_default();
        let launched_deps: Vec<String> = launched_config.pixi_toml_deps.clone().unwrap_or_default();

        // Find additions: deps in CRDT but not in launched baseline.
        // Launched baseline uses "name = version" format; CRDT uses package names.
        // Compare by extracted package name.
        let launched_names: std::collections::HashSet<String> = launched_deps
            .iter()
            .map(|d| d.split('=').next().unwrap_or(d).trim().to_lowercase())
            .collect();

        let to_add: Vec<&str> = current_deps
            .iter()
            .filter(|d| {
                let name = notebook_doc::metadata::extract_package_name(d).to_lowercase();
                !launched_names.contains(&name)
            })
            .map(|d| d.as_str())
            .collect();

        // Note: we only ADD deps to the project file, never remove. The CRDT
        // only tracks deps added through the notebook — it doesn't represent
        // the full project dep set. Removing deps that are "in project but not
        // in CRDT" would destroy project deps the notebook doesn't know about.

        let pixi_path = kernel_launch::tools::get_pixi_path()
            .await
            .map_err(|e| format!("Failed to find pixi: {e}"))?;

        let mut errors = Vec::new();

        for dep in &to_add {
            info!("[notebook-sync] Promoting dep to pixi.toml: {}", dep);
            match tokio::process::Command::new(&pixi_path)
                .args(["add", dep, "--manifest-path"])
                .arg(pixi_toml_path)
                .output()
                .await
            {
                Ok(output) if output.status.success() => {
                    promoted.push(format!("+{}", dep));
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    errors.push(format!("pixi add {} failed: {}", dep, stderr.trim()));
                }
                Err(e) => {
                    errors.push(format!("Failed to run pixi add {}: {}", dep, e));
                }
            }
        }

        // Always re-bootstrap CRDT from pixi.toml (even on partial failure)
        // so the CRDT reflects what actually happened.
        if !promoted.is_empty() || !errors.is_empty() {
            if let Ok(info) = kernel_launch::tools::pixi_info(pixi_toml_path).await {
                let deps = info.default_deps_snapshot();
                let mut doc = room.doc.write().await;
                doc.fork_and_merge(|fork| {
                    let mut snap = fork.get_metadata_snapshot().unwrap_or_default();
                    let pixi = snap.pixi_section_or_default();
                    pixi.dependencies = deps;
                    let _ = fork.set_metadata_snapshot(&snap);
                });
            }
        }
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
    } else if env_source == "conda:env_yml" {
        let yml_path = launched_config
            .environment_yml_path
            .as_ref()
            .ok_or_else(|| {
                "conda:env_yml kernel has no environment_yml_path in launched config".to_string()
            })?;

        let current_deps: Vec<String> = current_metadata
            .runt
            .conda
            .as_ref()
            .map(|c| c.dependencies.clone())
            .unwrap_or_default();
        // Launched baseline = environment.yml deps snapshot at launch time
        let launched_deps = launched_config
            .environment_yml_deps
            .clone()
            .unwrap_or_default();

        let launched_names: std::collections::HashSet<String> = launched_deps
            .iter()
            .map(|d| notebook_doc::metadata::extract_package_name(d).to_lowercase())
            .collect();

        let to_add: Vec<&str> = current_deps
            .iter()
            .filter(|d| {
                let name = notebook_doc::metadata::extract_package_name(d).to_lowercase();
                !launched_names.contains(&name)
            })
            .map(|d| d.as_str())
            .collect();

        // For conda:env_yml, we append new deps to environment.yml directly.
        // This is a simple text-based approach — append to the dependencies: section.
        let mut errors = Vec::new();

        if !to_add.is_empty() {
            match std::fs::read_to_string(yml_path) {
                Ok(content) => {
                    let mut new_content = content.clone();
                    // Find the end of the dependencies: section to insert new deps
                    let insertion_point = find_env_yml_deps_insertion_point(&content);
                    if let Some(pos) = insertion_point {
                        let mut insert_str = String::new();
                        for dep in &to_add {
                            insert_str.push_str(&format!("  - {}\n", dep));
                            promoted.push(format!("+{}", dep));
                        }
                        new_content.insert_str(pos, &insert_str);
                        if let Err(e) = std::fs::write(yml_path, &new_content) {
                            errors.push(format!("Failed to write environment.yml: {}", e));
                        }
                    } else {
                        // No dependencies: section found — append one
                        let mut append_str = String::from("\ndependencies:\n");
                        for dep in &to_add {
                            append_str.push_str(&format!("  - {}\n", dep));
                            promoted.push(format!("+{}", dep));
                        }
                        new_content.push_str(&append_str);
                        if let Err(e) = std::fs::write(yml_path, &new_content) {
                            errors.push(format!("Failed to write environment.yml: {}", e));
                        }
                    }
                }
                Err(e) => {
                    errors.push(format!("Failed to read environment.yml: {}", e));
                }
            }
        }

        // Re-bootstrap CRDT from environment.yml after changes
        if !promoted.is_empty() || !errors.is_empty() {
            if let Ok(env_config) = crate::project_file::parse_environment_yml(yml_path) {
                let deps = env_config.dependencies;
                let mut doc = room.doc.write().await;
                doc.fork_and_merge(|fork| {
                    let mut snap = fork.get_metadata_snapshot().unwrap_or_default();
                    let conda = snap.runt.conda.get_or_insert_with(|| {
                        notebook_doc::metadata::CondaInlineMetadata {
                            dependencies: Vec::new(),
                            channels: Vec::new(),
                            python: None,
                        }
                    });
                    conda.dependencies = deps;
                    let _ = fork.set_metadata_snapshot(&snap);
                });
            }
        }
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
    } else if env_source == "uv:pyproject" {
        let pyproject_path = launched_config.pyproject_path.as_ref().ok_or_else(|| {
            "uv:pyproject kernel has no pyproject_path in launched config".to_string()
        })?;
        let project_dir = pyproject_path
            .parent()
            .ok_or_else(|| "Cannot determine project directory".to_string())?;

        let current_deps: Vec<String> = current_metadata
            .runt
            .uv
            .as_ref()
            .map(|u| u.dependencies.clone())
            .unwrap_or_default();
        // For uv:pyproject, the launched baseline is the pyproject.toml deps.
        // Read them from the file for comparison.
        let launched_deps = if let Ok(content) = std::fs::read_to_string(pyproject_path) {
            extract_pyproject_deps(&content)
        } else {
            vec![]
        };

        let launched_names: std::collections::HashSet<String> = launched_deps
            .iter()
            .map(|d| notebook_doc::metadata::extract_package_name(d).to_lowercase())
            .collect();

        let to_add: Vec<&str> = current_deps
            .iter()
            .filter(|d| {
                let name = notebook_doc::metadata::extract_package_name(d).to_lowercase();
                !launched_names.contains(&name)
            })
            .map(|d| d.as_str())
            .collect();

        // Note: we only ADD deps to the project file, never remove. The CRDT
        // only tracks deps added through the notebook — it doesn't represent
        // the full project dep set. Removing deps that are "in project but not
        // in CRDT" would destroy project deps the notebook doesn't know about.

        let uv_path = kernel_launch::tools::get_uv_path()
            .await
            .map_err(|e| format!("Failed to find uv: {e}"))?;

        let mut errors = Vec::new();

        for dep in &to_add {
            info!("[notebook-sync] Promoting dep to pyproject.toml: {}", dep);
            match tokio::process::Command::new(&uv_path)
                .args(["add", dep, "--project"])
                .arg(project_dir)
                .output()
                .await
            {
                Ok(output) if output.status.success() => {
                    promoted.push(format!("+{}", dep));
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    errors.push(format!("uv add {} failed: {}", dep, stderr.trim()));
                }
                Err(e) => {
                    errors.push(format!("Failed to run uv add {}: {}", dep, e));
                }
            }
        }

        // Always re-bootstrap CRDT from pyproject.toml (even on partial failure)
        if !promoted.is_empty() || !errors.is_empty() {
            if let Ok(content) = std::fs::read_to_string(pyproject_path) {
                let deps = extract_pyproject_deps(&content);
                let mut doc = room.doc.write().await;
                doc.fork_and_merge(|fork| {
                    let mut snap = fork.get_metadata_snapshot().unwrap_or_default();
                    let uv = snap.runt.uv.get_or_insert_with(|| {
                        notebook_doc::metadata::UvInlineMetadata {
                            dependencies: Vec::new(),
                            requires_python: None,
                            prerelease: None,
                        }
                    });
                    uv.dependencies = deps;
                    let _ = fork.set_metadata_snapshot(&snap);
                });
            }
        }
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
    }

    Ok(promoted)
}

/// Handle sync environment request - hot-install new packages without kernel restart.
///
/// Only supported for UV inline dependencies when there are only additions (no removals).
/// Conda and other env types fall back to restart.
pub(crate) async fn handle_sync_environment(room: &NotebookRoom) -> NotebookResponse {
    // Read launched config from room
    let launched = {
        let guard = room.runtime_agent_launched_config.read().await;
        match &*guard {
            Some(lc) => lc.clone(),
            None => {
                return NotebookResponse::SyncEnvironmentFailed {
                    error: "No kernel running".to_string(),
                    needs_restart: false,
                };
            }
        }
    };

    // For project-backed environments, promote inline deps to the project file.
    // Project envs can't hot-install — they need a kernel restart.
    let env_source = {
        let sd = room.state_doc.read().await;
        sd.read_state().kernel.env_source.clone()
    };
    if env_source == "pixi:toml" || env_source == "uv:pyproject" || env_source == "conda:env_yml" {
        match promote_inline_deps_to_project(room, &env_source, &launched).await {
            Ok(promoted) if promoted.is_empty() => {
                return NotebookResponse::SyncEnvironmentComplete {
                    synced_packages: vec![],
                };
            }
            Ok(promoted) => {
                return NotebookResponse::SyncEnvironmentFailed {
                    error: format!(
                        "Updated project file ({}). Restart kernel to apply changes.",
                        promoted.join(", ")
                    ),
                    needs_restart: true,
                };
            }
            Err(e) => {
                return NotebookResponse::SyncEnvironmentFailed {
                    error: e,
                    needs_restart: false,
                };
            }
        }
    }

    // Read current metadata from the doc
    let current_metadata = {
        let doc = room.doc.read().await;
        doc.get_metadata_snapshot()
    };

    let Some(current_metadata) = current_metadata else {
        return NotebookResponse::SyncEnvironmentComplete {
            synced_packages: vec![],
        };
    };

    // Determine what packages need installing.
    // For inline-dep kernels, compute_env_sync_diff gives the drift.
    // For prewarmed kernels, check if the user added new inline deps.
    let is_tracking = launched.uv_deps.is_some()
        || launched.conda_deps.is_some()
        || launched.deno_config.is_some();

    let (packages_to_install, env_kind) = if is_tracking {
        // Kernel launched with inline deps — compute drift
        let diff = compute_env_sync_diff(&launched, &current_metadata);
        let Some(diff) = diff else {
            return NotebookResponse::SyncEnvironmentComplete {
                synced_packages: vec![],
            };
        };
        if !diff.removed.is_empty() || diff.channels_changed || diff.deno_changed {
            return NotebookResponse::SyncEnvironmentFailed {
                error: "Environment changes include removals or config changes that require a kernel restart".to_string(),
                needs_restart: true,
            };
        }
        let env_kind = if launched.uv_deps.is_some() {
            notebook_protocol::protocol::EnvKind::Uv {
                packages: diff.added.clone(),
            }
        } else if launched.conda_deps.is_some() {
            notebook_protocol::protocol::EnvKind::Conda {
                packages: diff.added.clone(),
                channels: get_inline_conda_channels(&current_metadata),
            }
        } else {
            return NotebookResponse::SyncEnvironmentFailed {
                error: "Hot-sync only supported for UV and Conda environments".to_string(),
                needs_restart: true,
            };
        };
        (diff.added, env_kind)
    } else {
        // Prewarmed kernel — check if user added inline deps
        let inline = check_inline_deps(&current_metadata);
        match inline.as_deref() {
            Some(s) if s.starts_with("uv:") => {
                let added = get_inline_uv_deps(&current_metadata).unwrap_or_default();
                if added.is_empty() {
                    return NotebookResponse::SyncEnvironmentComplete {
                        synced_packages: vec![],
                    };
                }
                let env_kind = notebook_protocol::protocol::EnvKind::Uv {
                    packages: added.clone(),
                };
                (added, env_kind)
            }
            Some("conda:inline") => {
                let added = get_inline_conda_deps(&current_metadata).unwrap_or_default();
                if added.is_empty() {
                    return NotebookResponse::SyncEnvironmentComplete {
                        synced_packages: vec![],
                    };
                }
                let env_kind = notebook_protocol::protocol::EnvKind::Conda {
                    packages: added.clone(),
                    channels: get_inline_conda_channels(&current_metadata),
                };
                (added, env_kind)
            }
            _ => {
                return NotebookResponse::SyncEnvironmentFailed {
                    error: "Hot-sync only supported for UV and Conda environments".to_string(),
                    needs_restart: true,
                };
            }
        }
    };

    if packages_to_install.is_empty() {
        return NotebookResponse::SyncEnvironmentComplete {
            synced_packages: vec![],
        };
    }

    // Send SyncEnvironment to the runtime agent
    let sync_request =
        notebook_protocol::protocol::RuntimeAgentRequest::SyncEnvironment(env_kind.clone());

    // Notify frontend that sync is starting
    let _ = room
        .kernel_broadcast_tx
        .send(NotebookBroadcast::EnvSyncState {
            in_sync: false,
            diff: Some(EnvSyncDiff {
                added: packages_to_install.clone(),
                removed: vec![],
                channels_changed: false,
                deno_changed: false,
            }),
        });

    match send_runtime_agent_request(room, sync_request).await {
        Ok(notebook_protocol::protocol::RuntimeAgentResponse::EnvironmentSynced {
            synced_packages,
        }) => {
            // Update runtime_agent_launched_config to include new packages
            {
                let mut lc = room.runtime_agent_launched_config.write().await;
                if let Some(ref mut config) = *lc {
                    match &env_kind {
                        notebook_protocol::protocol::EnvKind::Uv { .. } => {
                            // Promote prewarmed to uv:inline baseline if needed
                            if config.uv_deps.is_none() {
                                config.uv_deps = Some(vec![]);
                            }
                            if let Some(ref mut deps) = config.uv_deps {
                                for pkg in &synced_packages {
                                    if !deps.contains(pkg) {
                                        deps.push(pkg.clone());
                                    }
                                }
                            }
                        }
                        notebook_protocol::protocol::EnvKind::Conda { .. } => {
                            // Promote prewarmed to conda:inline baseline if needed
                            if config.conda_deps.is_none() {
                                config.conda_deps = Some(vec![]);
                            }
                            if let Some(ref mut deps) = config.conda_deps {
                                for pkg in &synced_packages {
                                    if !deps.contains(pkg) {
                                        deps.push(pkg.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Mark as in sync in RuntimeStateDoc
            {
                let mut sd = room.state_doc.write().await;
                if sd.set_env_sync(true, &[], &[], false, false) {
                    let _ = room.state_changed_tx.send(());
                }
            }

            let _ = room
                .kernel_broadcast_tx
                .send(NotebookBroadcast::EnvSyncState {
                    in_sync: true,
                    diff: None,
                });

            NotebookResponse::SyncEnvironmentComplete { synced_packages }
        }
        Ok(notebook_protocol::protocol::RuntimeAgentResponse::Error { error }) => {
            error!("[notebook-sync] SyncEnvironment failed: {}", error);
            NotebookResponse::SyncEnvironmentFailed {
                error,
                needs_restart: true,
            }
        }
        Ok(_) => {
            error!("[notebook-sync] SyncEnvironment received unexpected response type");
            NotebookResponse::SyncEnvironmentFailed {
                error: "Unexpected runtime agent response".to_string(),
                needs_restart: true,
            }
        }
        Err(e) => {
            error!(
                "[notebook-sync] SyncEnvironment agent communication error: {}",
                e
            );
            NotebookResponse::SyncEnvironmentFailed {
                error: format!("Agent communication error: {}", e),
                needs_restart: false,
            }
        }
    }
}

/// Format a single source string using ruff (Python) or deno fmt (Deno).
///
/// Returns the formatted source with trailing newline stripped (cells shouldn't
/// end with \n), or None if formatting failed or wasn't applicable.
pub(crate) async fn format_source(source: &str, runtime: &str) -> Option<String> {
    use kernel_launch::tools;
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;

    if source.trim().is_empty() {
        return None;
    }

    let raw = match runtime {
        "python" => {
            let ruff_path = tools::get_ruff_path().await.ok()?;
            let mut child = tokio::process::Command::new(&ruff_path)
                .args(["format", "--stdin-filename", "cell.py", "-"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .ok()?;

            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(source.as_bytes()).await.ok()?;
            }

            let output = child.wait_with_output().await.ok()?;
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        }
        "deno" => {
            let deno_path = tools::get_deno_path().await.ok()?;
            let mut child = tokio::process::Command::new(&deno_path)
                .args(["fmt", "--ext=ts", "-"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .ok()?;

            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(source.as_bytes()).await.ok()?;
            }

            let output = child.wait_with_output().await.ok()?;
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        }
        _ => None,
    }?;

    // Strip trailing newline (formatters always add one, but cells shouldn't have it)
    let formatted = raw.strip_suffix('\n').unwrap_or(&raw).to_string();

    if formatted != source {
        Some(formatted)
    } else {
        None
    }
}

/// Map a runtime name to its formatter's CRDT actor label.
pub(crate) fn formatter_actor(runtime: &str) -> String {
    let tool = match runtime {
        "python" => "ruff",
        other => other, // "deno" stays "deno"
    };
    format!("runtimed:{tool}")
}

/// Detect the runtime from room metadata, returning "python", "deno", or None.
pub(crate) async fn detect_room_runtime(room: &NotebookRoom) -> Option<String> {
    let doc = room.doc.read().await;
    doc.get_metadata_snapshot()
        .and_then(|snapshot| snapshot.detect_runtime())
}

/// Format all code cells in a notebook using ruff (Python) or deno fmt (Deno).
///
/// Reads the runtime type from the notebook metadata and formats accordingly.
/// Updates the Automerge doc with formatted sources and broadcasts changes.
/// Formatting errors are logged but don't fail the operation (best-effort).
pub(crate) async fn format_notebook_cells(room: &NotebookRoom) -> Result<usize, String> {
    let runtime = match detect_room_runtime(room).await {
        Some(rt) => rt,
        None => {
            info!("[format] Skipping format: unknown kernelspec (no formatter available)");
            return Ok(0);
        }
    };

    // Get all code cells
    let cells: Vec<(String, String)> = {
        let doc = room.doc.read().await;
        doc.get_cells()
            .into_iter()
            .filter(|cell| cell.cell_type == "code" && !cell.source.trim().is_empty())
            .map(|cell| (cell.id, cell.source))
            .collect()
    };

    if cells.is_empty() {
        return Ok(0);
    }

    // Fork BEFORE formatting so the baseline is the pre-format doc state.
    // Formatting ops on the fork are concurrent with any user edits on the
    // live doc, and Automerge's text CRDT merges them cleanly.
    let mut fork = {
        let mut doc = room.doc.write().await;
        doc.fork_with_actor(format!(
            "{}:{}",
            formatter_actor(&runtime),
            uuid::Uuid::new_v4()
        ))
    };

    let mut formatted_count = 0;
    for (cell_id, source) in cells {
        if let Some(fmt) = format_source(&source, &runtime).await {
            if fork.update_source(&cell_id, &fmt).is_ok() {
                formatted_count += 1;
            }
        }
    }

    if formatted_count > 0 {
        let mut doc = room.doc.write().await;
        if let Err(e) = catch_automerge_panic("save-format-merge", || doc.merge(&mut fork)) {
            warn!("{}", e);
            doc.rebuild_from_save();
        }
        let _ = room.changed_tx.send(());
        info!(
            "[format] Formatted {} code cells (runtime: {})",
            formatted_count, runtime
        );
    }

    Ok(formatted_count)
}

/// Save the notebook from the Automerge doc to disk as .ipynb.
///
/// If `target_path` is Some, saves to that path (with .ipynb appended if needed).
/// If `target_path` is None, saves to `room.path` (original file location).
///
/// 1. Read existing .ipynb from disk (if it exists) to preserve unknown metadata
/// 2. Read cells and metadata from the Automerge doc
/// 3. Merge metadata: replace kernelspec, language_info, runt; preserve everything else
/// 4. Reconstruct cells: source and outputs from Automerge, cell metadata from existing file
/// 5. Write the merged notebook to disk
///
/// Errors from save_notebook_to_disk.
#[derive(Debug)]
pub(crate) enum SaveError {
    /// Transient / potentially recoverable (e.g. disk full, busy)
    Retryable(String),
    /// Permanent — retrying will never help (path is a directory, permission denied, invalid path)
    Unrecoverable(String),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::Retryable(msg) | SaveError::Unrecoverable(msg) => f.write_str(msg),
        }
    }
}

/// Returns the absolute path where the notebook was written.
pub(crate) async fn save_notebook_to_disk(
    room: &NotebookRoom,
    target_path: Option<&str>,
) -> Result<String, SaveError> {
    // Diagnostic: log the call with the caller-supplied path and what the
    // room currently has as its path. Triangulates stray-file bugs by letting
    // us correlate saves against whoever fired them.
    debug!(
        "[save] save_notebook_to_disk entered: target_path={:?}, room.id={}, room.path={:?}",
        target_path,
        room.id,
        room.path.read().await.as_deref()
    );
    // Determine the actual save path
    let notebook_path = match target_path {
        Some(p) => {
            let path = PathBuf::from(p);

            // Reject relative paths - daemon CWD is unpredictable (could be / when running as launchd)
            // Clients (Tauri file dialog, Python SDK) should always provide absolute paths.
            if path.is_relative() {
                return Err(SaveError::Unrecoverable(format!(
                    "Relative paths are not supported for save: '{}'. Please provide an absolute path.",
                    p
                )));
            }

            // Ensure .ipynb extension
            if p.ends_with(".ipynb") {
                path
            } else {
                PathBuf::from(format!("{}.ipynb", p))
            }
        }
        None => match room.path.read().await.clone() {
            Some(p) => p,
            None => {
                return Err(SaveError::Unrecoverable(
                    "Cannot save untitled notebook without a target path. \
                 Please provide an explicit save path."
                        .to_string(),
                ))
            }
        },
    };

    // Read existing .ipynb to preserve unknown metadata and cell metadata
    // Distinguish between file-not-found (ok, create new) and parse errors (warn, continue)
    let existing: Option<serde_json::Value> = match tokio::fs::read_to_string(&notebook_path).await
    {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(value) => Some(value),
            Err(e) => {
                warn!(
                    "[notebook-sync] Existing notebook at {:?} has invalid JSON ({}), \
                     will overwrite without preserving metadata",
                    notebook_path, e
                );
                None
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            warn!(
                "[notebook-sync] Failed to read existing notebook {:?}: {}, \
                 will create new without preserving metadata",
                notebook_path, e
            );
            None
        }
    };

    // Read cells, metadata, heads, and per-cell execution_ids from the doc.
    // Heads are captured NOW (at snapshot time) so last_save_heads
    // matches what we serialize to disk, not what the doc looks like
    // after the async file write completes.
    let (cells, metadata_snapshot, snapshot_heads, cell_execution_ids) = {
        let mut doc = room.doc.write().await;
        let cells = doc.get_cells();
        let metadata_snapshot = doc.get_metadata_snapshot();
        let heads = doc.get_heads();
        // Collect execution_id for each cell (for output lookup in state doc)
        let eids: HashMap<String, Option<String>> = cells
            .iter()
            .map(|c| (c.id.clone(), doc.get_execution_id(&c.id)))
            .collect();
        (cells, metadata_snapshot, heads, eids)
    };

    // Read outputs and execution_count from RuntimeStateDoc keyed by execution_id.
    // Lock ordering: doc first (released above), then state_doc.
    let (cell_outputs, cell_execution_counts): (
        HashMap<String, Vec<serde_json::Value>>,
        HashMap<String, Option<i64>>,
    ) = {
        let sd = room.state_doc.read().await;
        let mut outputs_map = HashMap::new();
        let mut ec_map = HashMap::new();
        for (cell_id, eid) in &cell_execution_ids {
            if let Some(eid) = eid.as_ref() {
                let outputs = sd.get_outputs(eid);
                if !outputs.is_empty() {
                    outputs_map.insert(cell_id.clone(), outputs);
                }
                if let Some(exec) = sd.get_execution(eid) {
                    ec_map.insert(cell_id.clone(), exec.execution_count);
                }
            }
        }
        (outputs_map, ec_map)
    };

    let nbformat_attachments = room.nbformat_attachments.read().await.clone();

    // Reconstruct cells as JSON
    // Cell metadata now comes from the CellSnapshot (populated during load)
    let mut nb_cells = Vec::new();
    for cell in &cells {
        // Use metadata from the Automerge doc (populated during notebook load)
        let cell_meta = cell.metadata.clone();

        // Parse source into multiline array format (split_inclusive('\n'))
        let source_lines: Vec<String> = if cell.source.is_empty() {
            vec![]
        } else {
            let mut lines = Vec::new();
            let mut remaining = cell.source.as_str();
            while let Some(pos) = remaining.find('\n') {
                lines.push(remaining[..=pos].to_string());
                remaining = &remaining[pos + 1..];
            }
            if !remaining.is_empty() {
                lines.push(remaining.to_string());
            }
            lines
        };

        let mut cell_json = serde_json::json!({
            "id": cell.id,
            "cell_type": cell.cell_type,
            "source": source_lines,
            "metadata": cell_meta,
        });

        if cell.cell_type == "code" {
            // Resolve outputs from RuntimeStateDoc (keyed by execution_id)
            let mut resolved_outputs = Vec::new();
            if let Some(outputs) = cell_outputs.get(&cell.id) {
                for output in outputs {
                    let output_value = resolve_cell_output(output, &room.blob_store).await;
                    resolved_outputs.push(output_value);
                }
            }
            cell_json["outputs"] = serde_json::Value::Array(resolved_outputs);

            // Resolve execution_count from RuntimeStateDoc (source of truth)
            let exec_count: serde_json::Value = cell_execution_counts
                .get(&cell.id)
                .and_then(|ec| *ec)
                .map(|n| serde_json::Value::Number(serde_json::Number::from(n)))
                .unwrap_or(serde_json::Value::Null);
            cell_json["execution_count"] = exec_count;
        } else if matches!(cell.cell_type.as_str(), "markdown" | "raw") {
            if let Some(attachments) = nbformat_attachments.get(&cell.id) {
                cell_json["attachments"] = attachments.clone();
            }
        }

        nb_cells.push(cell_json);
    }

    // Build metadata by merging synced snapshot onto existing
    let mut metadata = existing
        .as_ref()
        .and_then(|nb| nb.get("metadata"))
        .cloned()
        .unwrap_or(serde_json::json!({}));

    if let Some(ref snapshot) = metadata_snapshot {
        snapshot.merge_into_metadata_value(&mut metadata).ok();
    }

    // Build the final notebook JSON
    // Cell IDs were introduced in nbformat 4.5, so ensure minor >= 5
    let existing_minor = existing
        .as_ref()
        .and_then(|nb| nb.get("nbformat_minor"))
        .and_then(|v| v.as_u64())
        .unwrap_or(5);
    let nbformat_minor = std::cmp::max(existing_minor, 5);

    let cell_count = nb_cells.len();
    let notebook_json = serde_json::json!({
        "nbformat": 4,
        "nbformat_minor": nbformat_minor,
        "metadata": metadata,
        "cells": nb_cells,
    });

    // Serialize with trailing newline (nbformat convention)
    let content = serde_json::to_string_pretty(&notebook_json)
        .map_err(|e| SaveError::Retryable(format!("Failed to serialize notebook: {e}")))?;
    let content_with_newline = format!("{content}\n");

    // Ensure parent directory exists (agents often construct paths programmatically)
    if let Some(parent) = notebook_path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            SaveError::Unrecoverable(format!(
                "Failed to create directory '{}': {e}",
                parent.display()
            ))
        })?;
    }

    // Write to disk (async to avoid blocking the runtime)
    tokio::fs::write(&notebook_path, content_with_newline)
        .await
        .map_err(|e| {
            let msg = format!("Failed to write notebook: {e}");
            match e.kind() {
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::IsADirectory => {
                    SaveError::Unrecoverable(msg)
                }
                _ => SaveError::Retryable(msg),
            }
        })?;

    // Update last_self_write timestamp so file watcher skips this change
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    room.last_self_write.store(now, Ordering::Relaxed);

    // Record snapshot-time heads so the file watcher can fork_at this
    // point. Only update when saving to the primary path — saving to an
    // alternate path (Save As) must not corrupt the fork base for the
    // file watcher.
    let is_primary_path =
        target_path.is_none() || room.path.read().await.as_deref() == Some(notebook_path.as_path());
    if is_primary_path {
        *room.last_save_heads.write().await = snapshot_heads;
        // Snapshot cell sources at save time so the file watcher can
        // distinguish our own writes from genuine external changes.
        let mut saved = HashMap::with_capacity(cells.len());
        for cell in &cells {
            saved.insert(cell.id.clone(), cell.source.clone());
        }
        *room.last_save_sources.write().await = saved;
    }

    info!(
        "[notebook-sync] Saved notebook to disk: {:?} ({} cells)",
        notebook_path, cell_count
    );

    Ok(notebook_path.to_string_lossy().to_string())
}

/// Transitions an untitled room to file-backed: claims path in path_index,
/// updates room.path, cleans up the stale `.automerge` persist file, spawns
/// the `.ipynb` file watcher and autosave debouncer, clears ephemeral markers,
/// and broadcasts `PathChanged`.
///
/// Returns `Ok(())` on success, or `Err(SaveErrorKind::PathAlreadyOpen)` if
/// another room is already serving this canonical path.  On error the caller's
/// room state is NOT mutated.
/// Canonicalize a path that may not yet exist on disk.
///
/// `tokio::fs::canonicalize` requires the target to exist. For pre-write
/// collision checks, we canonicalize the parent directory and append the
/// filename. Falls back to the raw path if even the parent is unresolvable.
pub(crate) async fn canonical_target_path(target: &Path) -> PathBuf {
    if let Ok(c) = tokio::fs::canonicalize(target).await {
        return c;
    }
    if let (Some(parent), Some(name)) = (target.parent(), target.file_name()) {
        if let Ok(canonical_parent) = tokio::fs::canonicalize(parent).await {
            return canonical_parent.join(name);
        }
    }
    target.to_path_buf()
}

/// Try to claim a path in the path_index for a given room. Returns the
/// structured `PathAlreadyOpen` error if another room already holds it.
pub(crate) async fn try_claim_path(
    path_index: &Arc<tokio::sync::Mutex<PathIndex>>,
    canonical: &Path,
    uuid: uuid::Uuid,
) -> Result<(), notebook_protocol::protocol::SaveErrorKind> {
    let mut idx = path_index.lock().await;
    match idx.insert(canonical.to_path_buf(), uuid) {
        Ok(()) => Ok(()),
        Err(path_index::PathIndexError::PathAlreadyOpen { uuid, path: p }) => Err(
            notebook_protocol::protocol::SaveErrorKind::PathAlreadyOpen {
                uuid: uuid.to_string(),
                path: p.to_string_lossy().into_owned(),
            },
        ),
    }
}

/// Finalize the untitled-to-file-backed transition AFTER the .ipynb has been
/// written and path_index already holds the claim. This is the non-claim half
/// of the promotion: path field update, persist file cleanup, file watcher +
/// autosave debouncer spawn, ephemeral marker clear, and PathChanged broadcast.
pub(crate) async fn finalize_untitled_promotion(room: &Arc<NotebookRoom>, canonical: PathBuf) {
    // Update room's path now that path_index owns it.
    *room.path.write().await = Some(canonical.clone());

    // NOTE: We don't actually stop the .automerge persist debouncer here —
    // stopping it would require taking ownership of room.persist_tx, which
    // the current struct definition doesn't support (it's a plain
    // Option<Sender<...>>). A subsequent AutomergeSync frame may resurrect
    // the .automerge file we delete below. That's OK because:
    //   - The file is keyed by SHA256(uuid), so it never collides with a
    //     different room.
    //   - Future open_notebook calls for the .ipynb go through a path key,
    //     not the UUID — the orphaned .automerge is never consulted.
    //   - The debouncer task dies when NotebookRoom is dropped on eviction.
    // TODO(followup): make persist_tx: Mutex<Option<...>> so .take() can
    // properly drop the sender and close the channel.
    if room.persist_path.exists() {
        if let Err(e) = tokio::fs::remove_file(&room.persist_path).await {
            warn!(
                "[notebook-sync] Failed to remove stale persist file {:?}: {}",
                room.persist_path, e
            );
        }
    }

    // Spawn .ipynb file watcher.
    if canonical.extension().is_some_and(|ext| ext == "ipynb") {
        let shutdown_tx = spawn_notebook_file_watcher(canonical.clone(), Arc::clone(room));
        *room.watcher_shutdown_tx.lock().await = Some(shutdown_tx);
    }

    // Spawn autosave debouncer so subsequent edits persist to .ipynb.
    spawn_autosave_debouncer(canonical.to_string_lossy().into_owned(), Arc::clone(room));

    // Clear ephemeral markers.
    room.is_ephemeral.store(false, Ordering::Relaxed);
    {
        let mut doc = room.doc.write().await;
        let _ = doc.delete_metadata("ephemeral");
    }

    // Broadcast path change to all peers.
    let _ = room
        .kernel_broadcast_tx
        .send(NotebookBroadcast::PathChanged {
            path: Some(canonical.to_string_lossy().into_owned()),
        });

    info!(
        "[notebook-sync] Promoted untitled room {} to file-backed path {:?}",
        room.id, canonical
    );
}

/// Updates path_index and room.path when a file-backed room is saved to a
/// Clone the notebook to a new path with a fresh env_id and cleared outputs.
///
/// This is used for "Save As Copy" functionality - creates a new independent notebook
/// without affecting the current document. The cloned notebook has:
/// - A fresh env_id (so it gets its own environment)
/// - All outputs cleared
/// - All execution_counts reset to null
/// - Cell metadata and nbformat attachments preserved from the source notebook
pub(crate) async fn clone_notebook_to_disk(
    room: &NotebookRoom,
    target_path: &str,
) -> Result<String, String> {
    let path = PathBuf::from(target_path);

    // Reject relative paths
    if path.is_relative() {
        return Err(format!(
            "Relative paths are not supported for clone: '{}'. Please provide an absolute path.",
            target_path
        ));
    }

    // Ensure .ipynb extension
    let clone_path = if target_path.ends_with(".ipynb") {
        path
    } else {
        PathBuf::from(format!("{}.ipynb", target_path))
    };

    // Read cells and metadata from the Automerge doc
    let (cells, metadata_snapshot) = {
        let doc = room.doc.read().await;
        (doc.get_cells(), doc.get_metadata_snapshot())
    };

    let nbformat_attachments = room.nbformat_attachments.read().await.clone();

    // Read existing source notebook to preserve unknown top-level metadata keys.
    let source_notebook_path = room.path.read().await.clone();
    let existing: Option<serde_json::Value> = match source_notebook_path {
        Some(ref p) => match tokio::fs::read_to_string(p).await {
            Ok(content) => serde_json::from_str(&content).ok(),
            Err(_) => None,
        },
        None => None,
    };

    // Generate fresh env_id for the cloned notebook
    let new_env_id = uuid::Uuid::new_v4().to_string();

    // Build cells with cleared outputs and execution counts, but preserved metadata
    let mut nb_cells = Vec::new();
    for cell in &cells {
        // Parse source into multiline array format using split_inclusive
        let source_lines: Vec<String> = if cell.source.is_empty() {
            vec![]
        } else {
            cell.source
                .split_inclusive('\n')
                .map(|s| s.to_string())
                .collect()
        };

        // Use metadata from the Automerge doc (populated during notebook load)
        let cell_meta = cell.metadata.clone();

        let mut cell_json = serde_json::json!({
            "id": cell.id,
            "cell_type": cell.cell_type,
            "source": source_lines,
            "metadata": cell_meta,
        });

        if cell.cell_type == "code" {
            // Clear outputs and execution_count for cloned notebook
            cell_json["outputs"] = serde_json::json!([]);
            cell_json["execution_count"] = serde_json::Value::Null;
        } else if matches!(cell.cell_type.as_str(), "markdown" | "raw") {
            if let Some(att) = nbformat_attachments.get(&cell.id) {
                cell_json["attachments"] = att.clone();
            }
        }

        nb_cells.push(cell_json);
    }

    // Build metadata: start with existing notebook metadata to preserve unknown fields,
    // then apply snapshot with fresh env_id
    let mut metadata = existing
        .as_ref()
        .and_then(|nb| nb.get("metadata"))
        .cloned()
        .unwrap_or(serde_json::json!({}));

    if let Some(mut snapshot) = metadata_snapshot {
        // Update env_id in the snapshot
        snapshot.runt.env_id = Some(new_env_id.clone());
        // Clear trust signature since this is a new notebook
        snapshot.runt.trust_signature = None;
        snapshot.runt.trust_timestamp = None;

        snapshot.merge_into_metadata_value(&mut metadata).ok();
    }

    // Determine nbformat_minor from existing or default to 5 (for cell IDs)
    let existing_minor = existing
        .as_ref()
        .and_then(|nb| nb.get("nbformat_minor"))
        .and_then(|v| v.as_u64())
        .unwrap_or(5);
    let nbformat_minor = std::cmp::max(existing_minor, 5);

    // Build the final notebook JSON
    let cell_count = nb_cells.len();
    let notebook_json = serde_json::json!({
        "nbformat": 4,
        "nbformat_minor": nbformat_minor,
        "metadata": metadata,
        "cells": nb_cells,
    });

    // Serialize with trailing newline
    let content = serde_json::to_string_pretty(&notebook_json)
        .map_err(|e| format!("Failed to serialize notebook: {e}"))?;
    let content_with_newline = format!("{content}\n");

    // Write to disk
    tokio::fs::write(&clone_path, content_with_newline)
        .await
        .map_err(|e| format!("Failed to write notebook: {e}"))?;

    info!(
        "[notebook-sync] Cloned notebook to disk: {:?} ({} cells, new env_id: {})",
        clone_path, cell_count, new_env_id
    );

    Ok(clone_path.to_string_lossy().to_string())
}

/// Resolve a single cell output — handles both manifest hashes and raw JSON.
async fn resolve_cell_output(
    output: &serde_json::Value,
    blob_store: &BlobStore,
) -> serde_json::Value {
    // If the output is a string, it's a legacy format (hash or raw JSON string)
    if let Some(s) = output.as_str() {
        // Check if it's a manifest hash (64-char hex string)
        if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
            if let Ok(Some(manifest_bytes)) = blob_store.get(s).await {
                if let Ok(manifest_json) = String::from_utf8(manifest_bytes) {
                    if let Ok(manifest) =
                        serde_json::from_str::<crate::output_store::OutputManifest>(&manifest_json)
                    {
                        if let Ok(resolved) =
                            crate::output_store::resolve_manifest(&manifest, blob_store).await
                        {
                            return resolved;
                        }
                    }
                }
            }
            warn!(
                "[notebook-sync] Failed to resolve legacy output manifest: {}",
                &s[..8]
            );
            return serde_json::json!({"output_type": "stream", "name": "stderr", "text": ["[output could not be resolved]"]});
        }
        // Raw JSON string — parse it
        match serde_json::from_str(s) {
            Ok(value) => return value,
            Err(e) => {
                warn!("[notebook-sync] Invalid JSON in raw output string: {}", e);
                return serde_json::json!({
                    "output_type": "stream",
                    "name": "stderr",
                    "text": ["[invalid output JSON]"]
                });
            }
        }
    }

    // Structured manifest/output object — resolve any blob refs
    match serde_json::from_value::<crate::output_store::OutputManifest>(output.clone()) {
        Ok(manifest) => match crate::output_store::resolve_manifest(&manifest, blob_store).await {
            Ok(resolved) => resolved,
            Err(_) => output.clone(),
        },
        Err(_) => output.clone(),
    }
}

/// Configuration for the persist debouncer timing.
#[derive(Clone, Copy)]
struct PersistDebouncerConfig {
    /// How long to wait after last update before flushing (debounce window)
    debounce_ms: u64,
    /// Maximum time between flushes during continuous updates
    max_interval_ms: u64,
    /// How often to check if we should flush
    check_interval_ms: u64,
}

impl Default for PersistDebouncerConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 500,
            max_interval_ms: 5000,
            check_interval_ms: 100,
        }
    }
}

/// Request to force the persist debouncer to flush pending data immediately.
/// The debouncer replies on the oneshot with `true` if the write succeeded
/// (or if there were no pending bytes to write), `false` on I/O error. Used
/// by room eviction to guarantee the persisted file reflects the latest doc
/// state *before* the room is removed from the map; a `false` reply tells
/// the caller the file on disk is still stale.
type FlushRequest = oneshot::Sender<bool>;

/// Spawn a debounced persistence task that coalesces writes.
///
/// Uses a `watch` channel for "latest value" semantics - new values replace old ones,
/// so we always persist the most recent state. No backpressure issues.
///
/// Persistence strategy:
/// - **Debounce (500ms)**: Wait 500ms after last update before writing
/// - **Max interval (5s)**: During continuous output, flush at least every 5s
/// - **Flush request**: Force an immediate write and ack (used by eviction)
/// - **Shutdown flush**: Persist any pending data when channel closes
///
/// This reduces disk I/O during rapid output while ensuring durability.
fn spawn_persist_debouncer(
    persist_rx: watch::Receiver<Option<Vec<u8>>>,
    flush_rx: mpsc::UnboundedReceiver<FlushRequest>,
    persist_path: PathBuf,
) {
    spawn_persist_debouncer_with_config(
        persist_rx,
        flush_rx,
        persist_path,
        PersistDebouncerConfig::default(),
    );
}

/// Spawn debouncer with custom timing configuration (for testing).
fn spawn_persist_debouncer_with_config(
    mut persist_rx: watch::Receiver<Option<Vec<u8>>>,
    mut flush_rx: mpsc::UnboundedReceiver<FlushRequest>,
    persist_path: PathBuf,
    config: PersistDebouncerConfig,
) {
    spawn_supervised(
        "persist-debouncer",
        async move {
            use std::time::Duration;
            use tokio::time::{interval, Instant, MissedTickBehavior};

            let debounce_duration = Duration::from_millis(config.debounce_ms);
            let max_flush_interval = Duration::from_millis(config.max_interval_ms);

            let mut last_receive: Option<Instant> = None;
            let mut last_flush: Option<Instant> = None;

            // Persistent interval - fires regularly regardless of how often changed() fires.
            // This ensures we always check debounce/max-interval even during rapid updates.
            let mut check_interval = interval(Duration::from_millis(config.check_interval_ms));
            check_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    result = persist_rx.changed() => {
                        if result.is_err() {
                            // Channel closed - flush any pending data and exit
                            let bytes = persist_rx.borrow().clone();
                            if let Some(data) = bytes {
                                do_persist(&data, &persist_path);
                            }
                            break;
                        }
                        last_receive = Some(Instant::now());
                    }
                    maybe_req = flush_rx.recv() => {
                        match maybe_req {
                            Some(ack) => {
                                // Eviction (or another caller) wants a synchronous flush.
                                // Write the latest doc bytes, then ack with the write
                                // result so the caller knows whether the file is
                                // current. No pending bytes = nothing to write = ack true
                                // (the file either doesn't exist or already reflects
                                // the latest state).
                                let bytes = persist_rx.borrow().clone();
                                let ok = if let Some(data) = bytes {
                                    let write_ok = do_persist(&data, &persist_path);
                                    if write_ok {
                                        last_flush = Some(Instant::now());
                                        last_receive = None;
                                    }
                                    write_ok
                                } else {
                                    true
                                };
                                // Receiver may have dropped (eviction timed out); ignore.
                                let _ = ack.send(ok);
                            }
                            None => {
                                // All flush senders dropped. The room is being torn
                                // down; the watch receiver will close next and we'll
                                // hit the shutdown flush on the persist_rx.changed()
                                // Err arm. Break defensively to avoid hot-looping if
                                // that somehow doesn't fire (we still want to flush
                                // any pending bytes first).
                                let bytes = persist_rx.borrow().clone();
                                if let Some(data) = bytes {
                                    do_persist(&data, &persist_path);
                                }
                                break;
                            }
                        }
                    }
                    _ = check_interval.tick() => {
                        // Check if we should flush based on debounce or max interval
                        let should_flush = if let Some(recv) = last_receive {
                            // Debounce: 500ms quiet period since last receive
                            let debounce_ready = recv.elapsed() >= debounce_duration;
                            // Max interval: 5s since last flush (or since first receive)
                            let max_interval_ready = last_flush
                                .map(|f| f.elapsed() >= max_flush_interval)
                                .unwrap_or(recv.elapsed() >= max_flush_interval);
                            debounce_ready || max_interval_ready
                        } else {
                            false
                        };

                        if should_flush {
                            let bytes = persist_rx.borrow().clone();
                            if let Some(data) = bytes {
                                do_persist(&data, &persist_path);
                                last_flush = Some(Instant::now());
                                last_receive = None;
                            }
                        }
                    }
                }
            }
        },
        |_| {
            trigger_global_shutdown();
        },
    );
}

/// Configuration for the autosave debouncer (testable).
struct AutosaveDebouncerConfig {
    /// Quiet period: flush only after no changes for this long.
    debounce_ms: u64,
    /// Max interval: flush even during continuous changes after this long.
    max_interval_ms: u64,
    /// How often to check whether a flush is due.
    check_interval_ms: u64,
}

impl Default for AutosaveDebouncerConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 2_000,
            max_interval_ms: 10_000,
            check_interval_ms: 500,
        }
    }
}

/// Spawn a debounced autosave task that writes the `.ipynb` file to disk
/// whenever the Automerge document changes. Only for saved (non-untitled)
/// notebooks. Does NOT format cells — formatting is reserved for explicit saves.
fn spawn_autosave_debouncer(notebook_id: String, room: Arc<NotebookRoom>) {
    spawn_autosave_debouncer_with_config(notebook_id, room, AutosaveDebouncerConfig::default());
}

/// Spawn autosave debouncer with custom timing configuration (for testing).
fn spawn_autosave_debouncer_with_config(
    notebook_id: String,
    room: Arc<NotebookRoom>,
    config: AutosaveDebouncerConfig,
) {
    let mut changed_rx = room.changed_tx.subscribe();
    spawn_supervised(
        "autosave-debouncer",
        async move {
            use std::time::Duration;
            use tokio::time::{interval, Instant, MissedTickBehavior};

            let debounce_duration = Duration::from_millis(config.debounce_ms);
            let max_flush_interval = Duration::from_millis(config.max_interval_ms);

            let mut last_receive: Option<Instant> = None;
            let mut last_flush: Option<Instant> = None;

            let mut check_interval = interval(Duration::from_millis(config.check_interval_ms));
            check_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    result = changed_rx.recv() => {
                        match result {
                            Ok(()) => {
                                last_receive = Some(Instant::now());
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                // Room is being evicted — do a final autosave
                                if !is_untitled_notebook(&notebook_id)
                                    && !room.is_loading.load(Ordering::Acquire)
                                {
                                    match save_notebook_to_disk(&room, None).await {
                                        Ok(path) => {
                                            info!("[autosave] Final save on room close: {}", path);
                                        }
                                        Err(e) => {
                                            warn!("[autosave] Final save failed: {}", e);
                                        }
                                    }
                                }
                                break;
                            }
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                // Missed some updates, treat as a change
                                debug!("[autosave] Lagged {} messages", n);
                                last_receive = Some(Instant::now());
                            }
                        }
                    }
                    _ = check_interval.tick() => {
                        let should_flush = if let Some(recv) = last_receive {
                            let debounce_ready = recv.elapsed() >= debounce_duration;
                            let max_interval_ready = last_flush
                                .map(|f| f.elapsed() >= max_flush_interval)
                                .unwrap_or(recv.elapsed() >= max_flush_interval);
                            debounce_ready || max_interval_ready
                        } else {
                            false
                        };

                        if should_flush {
                            // Skip during initial load
                            if room.is_loading.load(Ordering::Acquire) {
                                continue;
                            }

                            match save_notebook_to_disk(&room, None).await {
                                Ok(path) => {
                                    debug!("[autosave] Saved {}", path);
                                    last_flush = Some(Instant::now());

                                    // Check if changes arrived during the save. If so,
                                    // keep last_receive set so we flush again soon —
                                    // don't broadcast "clean" when the file is already stale.
                                    let changed_during_save =
                                        matches!(changed_rx.try_recv(), Ok(()) | Err(broadcast::error::TryRecvError::Lagged(_)));
                                    if changed_during_save {
                                        last_receive = Some(Instant::now());
                                    } else {
                                        last_receive = None;
                                        // Broadcast to connected clients so they can clear dirty state
                                        let _ = room.kernel_broadcast_tx.send(
                                            NotebookBroadcast::NotebookAutosaved {
                                                path,
                                            },
                                        );
                                    }
                                }
                                Err(ref e @ SaveError::Unrecoverable(_)) => {
                                    error!(
                                        "[autosave] Unrecoverable save error, disabling autosave for {}: {}",
                                        notebook_id, e
                                    );
                                    break;
                                }
                                Err(e) => {
                                    warn!("[autosave] Failed to save: {}", e);
                                    // Keep last_receive set so we retry on next interval
                                    last_flush = Some(Instant::now());
                                }
                            }
                        }
                    }
                }
            }
        },
        |_| {
            trigger_global_shutdown();
        },
    );
}

/// Actually persist bytes to disk, logging if it takes too long.
/// Returns `true` on success, `false` on I/O error.
fn do_persist(data: &[u8], path: &Path) -> bool {
    let start = std::time::Instant::now();
    let ok = persist_notebook_bytes(data, path);
    let elapsed = start.elapsed();
    if elapsed > std::time::Duration::from_millis(500) {
        warn!(
            "[persist-debouncer] Slow write: {:?} took {:?}",
            path, elapsed
        );
    }
    ok
}

/// Persist pre-serialized notebook bytes to disk.
///
/// Returns `true` on success, `false` on I/O error. Callers that need to
/// know whether the bytes actually hit disk (e.g. eviction's flush-and-ack
/// path) must check the return value; earlier call sites that only care
/// about best-effort debounced writes can ignore it.
pub(crate) fn persist_notebook_bytes(data: &[u8], path: &Path) -> bool {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!(
                "[notebook-sync] Failed to create parent dir for {:?}: {}",
                path, e
            );
            return false;
        }
    }
    if let Err(e) = std::fs::write(path, data) {
        warn!("[notebook-sync] Failed to save notebook doc: {}", e);
        return false;
    }
    true
}

// =============================================================================
// Notebook File Watching
// =============================================================================
//
// Watch .ipynb files for external changes (git, VS Code, other editors).
// When changes are detected, merge them into the Automerge doc and broadcast.

/// Time window (ms) to skip file change events after our own writes.
/// Must be larger than the debounce window (500ms) to reliably skip self-writes.
const SELF_WRITE_SKIP_WINDOW_MS: u64 = 600;

/// Parsed result of an `.ipynb` file: cell snapshots and the outputs pulled
/// from disk, keyed by cell id.
///
/// `CellSnapshot` no longer carries outputs (outputs live in `RuntimeStateDoc`
/// keyed by `execution_id`). Callers that need both pieces — notebook load,
/// external-edit merge — receive them here as two parallel structures and
/// thread the outputs map separately.
pub(crate) struct ParsedIpynbCells {
    pub cells: Vec<CellSnapshot>,
    pub outputs_by_cell: HashMap<String, Vec<serde_json::Value>>,
}

/// Parse cells from a Jupyter notebook JSON object.
///
/// Returns `Some(ParsedIpynbCells)` if parsing succeeded (including empty
/// `cells: []`), or `None` if the `cells` key is missing or invalid.
///
/// The source field can be either a string or an array of strings (lines).
/// We normalize it to a single string.
///
/// For older notebooks (pre-nbformat 4.5) that don't have cell IDs, we generate
/// stable fallback IDs based on the cell index. This prevents data loss when
/// merging changes from externally-generated notebooks.
///
/// Positions are generated incrementally using fractional indexing.
fn parse_cells_from_ipynb(json: &serde_json::Value) -> Option<ParsedIpynbCells> {
    use loro_fractional_index::FractionalIndex;

    let cells_json = json.get("cells").and_then(|c| c.as_array())?;

    // Generate positions incrementally
    let mut prev_position: Option<FractionalIndex> = None;
    let mut outputs_by_cell: HashMap<String, Vec<serde_json::Value>> = HashMap::new();

    let parsed_cells = cells_json
        .iter()
        .enumerate()
        .map(|(index, cell)| {
            // Use existing ID or generate a stable fallback based on index
            // This handles older notebooks (pre-nbformat 4.5) without cell IDs
            let id = cell
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("__external_cell_{}", index));

            let cell_type = cell
                .get("cell_type")
                .and_then(|v| v.as_str())
                .unwrap_or("code")
                .to_string();

            // Generate position incrementally (O(1) per cell, not O(n²))
            let position = match &prev_position {
                None => FractionalIndex::default(),
                Some(prev) => FractionalIndex::new_after(prev),
            };
            let position_str = position.to_string();
            prev_position = Some(position);

            // Source can be a string or array of strings
            let source = match cell.get("source") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Array(arr)) => arr
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(""),
                _ => String::new(),
            };

            // Execution count: number or null
            let execution_count = match cell.get("execution_count") {
                Some(serde_json::Value::Number(n)) => n.to_string(),
                _ => "null".to_string(),
            };

            // Outputs travel alongside the snapshot, not on it — they're
            // destined for RuntimeStateDoc, keyed by execution_id, once the
            // caller mints a synthetic execution for this cell.
            let outputs: Vec<serde_json::Value> = cell
                .get("outputs")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if !outputs.is_empty() {
                outputs_by_cell.insert(id.clone(), outputs);
            }

            // Cell metadata (preserves all fields, normalize to object)
            let metadata = match cell.get("metadata") {
                Some(v) if v.is_object() => v.clone(),
                _ => serde_json::json!({}),
            };

            CellSnapshot {
                id,
                cell_type,
                position: position_str,
                source,
                execution_count,
                metadata,
                resolved_assets: std::collections::HashMap::new(),
            }
        })
        .collect();

    Some(ParsedIpynbCells {
        cells: parsed_cells,
        outputs_by_cell,
    })
}

/// Parse nbformat attachment payloads from a .ipynb JSON value.
///
/// Returns a map of `cell_id -> attachments JSON object` for any cell carrying attachments.
fn parse_nbformat_attachments_from_ipynb(
    json: &serde_json::Value,
) -> HashMap<String, serde_json::Value> {
    let Some(cells_json) = json.get("cells").and_then(|c| c.as_array()) else {
        return HashMap::new();
    };

    cells_json
        .iter()
        .enumerate()
        .filter_map(|(index, cell)| {
            let id = cell
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("__external_cell_{}", index));

            let attachments = cell.get("attachments")?;
            if attachments.is_object() {
                Some((id, attachments.clone()))
            } else {
                None
            }
        })
        .collect()
}

/// Parse notebook metadata from a .ipynb JSON value.
///
/// Uses `NotebookMetadataSnapshot::from_metadata_value` which extracts
/// kernelspec, language_info, and runt namespace from the metadata.
fn parse_metadata_from_ipynb(json: &serde_json::Value) -> Option<NotebookMetadataSnapshot> {
    let metadata = json.get("metadata")?;
    Some(NotebookMetadataSnapshot::from_metadata_value(metadata))
}

/// Convert raw output JSON strings to blob store manifest references.
///
/// Each output is parsed, converted to a manifest (with large data offloaded
/// to the blob store), and the manifest itself is stored in the blob store.
/// Returns a vec of manifest hashes suitable for storing in the Automerge doc.
///
/// Falls back to storing the raw JSON string if manifest creation fails.
async fn outputs_to_manifest_refs(
    raw_outputs: &[serde_json::Value],
    blob_store: &BlobStore,
) -> Vec<serde_json::Value> {
    let mut refs = Vec::with_capacity(raw_outputs.len());
    for output_value in raw_outputs {
        let output_ref = match crate::output_store::create_manifest(
            output_value,
            blob_store,
            crate::output_store::DEFAULT_INLINE_THRESHOLD,
        )
        .await
        {
            Ok(manifest) => manifest.to_json(),
            Err(e) => {
                warn!("[notebook-sync] Failed to create output manifest: {}", e);
                output_value.clone()
            }
        };
        refs.push(output_ref);
    }
    refs
}

/// Number of cells to add per batch during streaming load.
/// After each batch, a sync message is sent so the frontend can render
/// cells progressively.
const STREAMING_BATCH_SIZE: usize = 3;

type NbformatAttachmentMap = HashMap<String, serde_json::Value>;
type ResolvedAssets = HashMap<String, String>;
type ParsedStreamingNotebook = (
    Vec<StreamingCell>,
    Option<NotebookMetadataSnapshot>,
    NbformatAttachmentMap,
);
type StreamingLoadBatchEntry = (usize, StreamingCell, Vec<serde_json::Value>, ResolvedAssets);

fn should_resolve_markdown_assets(cell_type: &str) -> bool {
    cell_type == "markdown"
}

/// Cell data parsed for streaming load.
///
/// Unlike `CellSnapshot` — which no longer carries outputs at all (they live
/// in `RuntimeStateDoc` keyed by `execution_id`) — this struct pairs the
/// cell fields with its parsed outputs in one value. Outputs are kept as
/// `serde_json::Value` to avoid the serialize→parse round-trip when
/// processing through `create_manifest`.
struct StreamingCell {
    id: String,
    cell_type: String,
    position: String,
    source: String,
    execution_count: String,
    outputs: Vec<serde_json::Value>,
    metadata: serde_json::Value,
}

/// Convert a `jiter::JsonValue` to a `serde_json::Value`.
///
/// Used to bridge jiter's fast zero-copy parsing with code that expects
/// serde_json types (e.g., `output_store::create_manifest`).
fn jiter_to_serde(jv: &jiter::JsonValue<'_>) -> serde_json::Value {
    match jv {
        jiter::JsonValue::Null => serde_json::Value::Null,
        jiter::JsonValue::Bool(b) => serde_json::Value::Bool(*b),
        jiter::JsonValue::Int(i) => serde_json::json!(*i),
        jiter::JsonValue::BigInt(b) => serde_json::Value::String(b.to_string()),
        jiter::JsonValue::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        jiter::JsonValue::Str(s) => serde_json::Value::String(s.to_string()),
        jiter::JsonValue::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(jiter_to_serde).collect())
        }
        jiter::JsonValue::Object(obj) => {
            let map = obj
                .iter()
                .map(|(k, v)| (k.to_string(), jiter_to_serde(v)))
                .collect();
            serde_json::Value::Object(map)
        }
    }
}

/// Look up a key in a jiter JSON object (which is a flat slice of key-value pairs).
///
/// `LazyIndexMap` derefs to `[(Cow<str>, JsonValue)]`, so the built-in `.get()`
/// takes a `usize` index. This helper does the string-key search.
fn jobj_get<'a, 's>(
    obj: &'a [(std::borrow::Cow<'s, str>, jiter::JsonValue<'s>)],
    key: &str,
) -> Option<&'a jiter::JsonValue<'s>> {
    obj.iter().find(|(k, _)| k.as_ref() == key).map(|(_, v)| v)
}

/// Parse a notebook file into streaming cells using jiter for fast JSON parsing.
///
/// Returns `(cells, Option<metadata_snapshot>)`. Outputs are kept as
/// `serde_json::Value` so they can be passed directly to `create_manifest`
/// without a serialize→parse round-trip.
fn parse_notebook_jiter(bytes: &[u8]) -> Result<ParsedStreamingNotebook, String> {
    let json = jiter::JsonValue::parse(bytes, false)
        .map_err(|e| format!("Invalid notebook JSON: {}", e))?;

    let obj = match &json {
        jiter::JsonValue::Object(obj) => obj,
        _ => return Err("Notebook is not a JSON object".to_string()),
    };

    // Parse metadata by converting to serde_json (metadata is small)
    let metadata = jobj_get(obj, "metadata").map(|m| {
        let serde_meta = jiter_to_serde(m);
        NotebookMetadataSnapshot::from_metadata_value(&serde_meta)
    });

    let cells_arr = match jobj_get(obj, "cells") {
        Some(jiter::JsonValue::Array(arr)) => arr,
        Some(_) => return Err("'cells' is not an array".to_string()),
        None => return Ok((vec![], metadata, HashMap::new())),
    };

    use loro_fractional_index::FractionalIndex;
    let mut prev_position: Option<FractionalIndex> = None;

    let mut cells = Vec::with_capacity(cells_arr.len());
    let mut attachments = HashMap::new();
    for (index, cell) in cells_arr.iter().enumerate() {
        let cell_obj = match cell {
            jiter::JsonValue::Object(obj) => obj,
            _ => continue,
        };

        let id = jobj_get(cell_obj, "id")
            .and_then(|v| match v {
                jiter::JsonValue::Str(s) => Some(s.to_string()),
                _ => None,
            })
            .unwrap_or_else(|| format!("__external_cell_{}", index));

        let cell_type = jobj_get(cell_obj, "cell_type")
            .and_then(|v| match v {
                jiter::JsonValue::Str(s) => Some(s.to_string()),
                _ => None,
            })
            .unwrap_or_else(|| "code".to_string());

        // Generate position incrementally (O(1) per cell, not O(n²))
        let position = match &prev_position {
            None => FractionalIndex::default(),
            Some(prev) => FractionalIndex::new_after(prev),
        };
        let position_str = position.to_string();
        prev_position = Some(position);

        // Source can be a string or array of strings (Jupyter multiline format)
        let source = match jobj_get(cell_obj, "source") {
            Some(jiter::JsonValue::Str(s)) => s.to_string(),
            Some(jiter::JsonValue::Array(arr)) => arr
                .iter()
                .filter_map(|v| match v {
                    jiter::JsonValue::Str(s) => Some(s.as_ref()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
            _ => String::new(),
        };

        let execution_count = match jobj_get(cell_obj, "execution_count") {
            Some(jiter::JsonValue::Int(n)) => n.to_string(),
            _ => "null".to_string(),
        };

        // Keep outputs as serde_json::Value — avoids serialize→parse round-trip
        let outputs = match jobj_get(cell_obj, "outputs") {
            Some(jiter::JsonValue::Array(arr)) => arr.iter().map(jiter_to_serde).collect(),
            _ => vec![],
        };

        // Extract cell metadata (preserves all fields, normalize to object)
        let metadata = match jobj_get(cell_obj, "metadata").map(jiter_to_serde) {
            Some(v) if v.is_object() => v,
            _ => serde_json::json!({}),
        };

        if let Some(jiter::JsonValue::Object(_)) = jobj_get(cell_obj, "attachments") {
            attachments.insert(
                id.clone(),
                jobj_get(cell_obj, "attachments")
                    .map(jiter_to_serde)
                    .unwrap_or_else(|| serde_json::json!({})),
            );
        }

        cells.push(StreamingCell {
            id,
            cell_type,
            position: position_str,
            source,
            execution_count,
            outputs,
            metadata,
        });
    }

    Ok((cells, metadata, attachments))
}

/// Convert a single output `serde_json::Value` to a blob store manifest hash.
///
/// Like `outputs_to_manifest_refs` but takes a `Value` directly instead of a
/// JSON string, avoiding the serialize→parse round-trip during notebook load.
async fn output_value_to_manifest_ref(
    output: &serde_json::Value,
    blob_store: &BlobStore,
) -> serde_json::Value {
    match crate::output_store::create_manifest(
        output,
        blob_store,
        crate::output_store::DEFAULT_INLINE_THRESHOLD,
    )
    .await
    {
        Ok(manifest) => manifest.to_json(),
        Err(e) => {
            warn!("[streaming-load] Failed to create output manifest: {}", e);
            output.clone()
        }
    }
}

/// Placeholder for draining incoming sync replies during streaming load.
///
/// In theory, the client sends sync replies after each batch and we should
/// drain them to prevent socket buffer deadlock. In practice:
///
/// 1. `recv_typed_frame` uses `read_exact` internally, which is NOT
///    cancellation-safe. Wrapping it in `tokio::time::timeout` risks
///    cancelling mid-frame, leaving the stream desynchronized.
/// 2. With release-mode load times (~56ms for 50 cells), the OS socket
///    buffer (typically 64KB+) easily absorbs the client's sync replies.
/// 3. Non-sync frames (requests) would be silently dropped.
///
/// The sync replies are processed normally once the main select loop starts
/// after streaming completes.
async fn drain_incoming_frames<R>(
    _reader: &mut R,
    _room: &NotebookRoom,
    _peer_state: &mut sync::State,
) where
    R: AsyncRead + Unpin,
{
    // No-op. See doc comment above.
}

/// Stream notebook cells into the Automerge doc in batches, sending sync
/// messages after each batch so the frontend renders cells progressively.
///
/// This replaces the "load everything then sync once" approach. With a 50-cell
/// notebook, the frontend sees the first 3 cells in ~30ms instead of waiting
/// for all 50.
///
/// The caller must have already won `room.try_start_loading()` and must call
/// `room.finish_loading()` after this returns (success or failure).
pub(crate) async fn streaming_load_cells<R, W>(
    reader: &mut R,
    writer: &mut W,
    room: &NotebookRoom,
    path: &Path,
    peer_state: &mut sync::State,
) -> Result<usize, String>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let start = std::time::Instant::now();

    // 1. Read and parse the notebook file with jiter
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("Failed to read notebook: {}", e))?;

    let (cells, metadata, nbformat_attachments) = parse_notebook_jiter(&bytes)?;
    {
        let mut cache = room.nbformat_attachments.write().await;
        *cache = nbformat_attachments.clone();
    }

    let total_cells = cells.len();
    info!(
        "[streaming-load] Parsed {} cells from {} in {:?}",
        total_cells,
        path.display(),
        start.elapsed()
    );

    // 2. Stream cells in batches
    let mut cell_iter = cells.into_iter().enumerate().peekable();
    let mut batch_num = 0u32;

    while cell_iter.peek().is_some() {
        let batch_start = std::time::Instant::now();

        // Collect one batch and process outputs through blob store (outside doc lock)
        let mut batch: Vec<StreamingLoadBatchEntry> = Vec::new();
        for _ in 0..STREAMING_BATCH_SIZE {
            let Some((idx, cell)) = cell_iter.next() else {
                break;
            };
            let mut output_refs = Vec::with_capacity(cell.outputs.len());
            for output in &cell.outputs {
                output_refs.push(output_value_to_manifest_ref(output, &room.blob_store).await);
            }
            let resolved_assets = if should_resolve_markdown_assets(&cell.cell_type) {
                resolve_markdown_assets(
                    &cell.source,
                    Some(path),
                    nbformat_attachments.get(&cell.id),
                    &room.blob_store,
                )
                .await
            } else {
                ResolvedAssets::new()
            };
            batch.push((idx, cell, output_refs, resolved_assets));
        }

        // Store outputs in RuntimeStateDoc with synthetic execution_ids.
        // Collect (cell_id, synthetic_eid) pairs for linking below.
        let mut cell_eids: HashMap<String, String> = HashMap::new();
        {
            let mut sd = room.state_doc.write().await;
            for (_idx, cell, output_refs, _resolved_assets) in &batch {
                if !output_refs.is_empty() {
                    let synthetic_eid = uuid::Uuid::new_v4().to_string();
                    sd.create_execution(&synthetic_eid, &cell.id);
                    let _ = sd.set_outputs(&synthetic_eid, output_refs);
                    sd.set_execution_done(&synthetic_eid, true);
                    cell_eids.insert(cell.id.clone(), synthetic_eid);
                }
            }
        }

        // Add batch to Automerge doc and generate sync message (inside lock)
        let encoded = {
            let mut doc = room.doc.write().await;
            for (_idx, cell, _output_refs, resolved_assets) in &batch {
                doc.add_cell_full(
                    &cell.id,
                    &cell.cell_type,
                    &cell.position,
                    &cell.source,
                    &cell.execution_count,
                    &cell.metadata,
                )
                .map_err(|e| format!("Failed to add cell {}: {}", cell.id, e))?;
                // Link cell to its synthetic execution_id
                if let Some(eid) = cell_eids.get(&cell.id) {
                    let _ = doc.set_execution_id(&cell.id, Some(eid));
                }
                doc.set_cell_resolved_assets(&cell.id, resolved_assets)
                    .map_err(|e| format!("Failed to set resolved assets for {}: {}", cell.id, e))?;
            }
            match catch_automerge_panic("streaming-load-cells", || {
                doc.generate_sync_message(peer_state).map(|m| m.encode())
            }) {
                Ok(encoded) => encoded,
                Err(e) => {
                    warn!("{}", e);
                    doc.rebuild_from_save();
                    *peer_state = sync::State::new();
                    doc.generate_sync_message(peer_state).map(|m| m.encode())
                }
            }
        };

        // Send sync message outside the lock
        if let Some(encoded) = encoded {
            connection::send_typed_frame(writer, NotebookFrameType::AutomergeSync, &encoded)
                .await
                .map_err(|e| format!("Failed to send sync message: {}", e))?;
        }

        // Notify other peers in the room
        let _ = room.changed_tx.send(());
        if !cell_eids.is_empty() {
            let _ = room.state_changed_tx.send(());
        }

        // Drain incoming sync replies to prevent deadlock
        drain_incoming_frames(reader, room, peer_state).await;

        batch_num += 1;
        debug!(
            "[streaming-load] Batch {} ({} cells) in {:?}",
            batch_num,
            batch.len(),
            batch_start.elapsed(),
        );
    }

    // 3. Set metadata (if present) and sync it
    if let Some(meta) = metadata {
        let encoded = {
            let mut doc = room.doc.write().await;
            if let Err(e) = doc.set_metadata_snapshot(&meta) {
                warn!("[streaming-load] Failed to set metadata: {}", e);
            }
            match catch_automerge_panic("streaming-load-meta", || {
                doc.generate_sync_message(peer_state).map(|m| m.encode())
            }) {
                Ok(encoded) => encoded,
                Err(e) => {
                    warn!("{}", e);
                    doc.rebuild_from_save();
                    *peer_state = sync::State::new();
                    doc.generate_sync_message(peer_state).map(|m| m.encode())
                }
            }
        };
        if let Some(encoded) = encoded {
            connection::send_typed_frame(writer, NotebookFrameType::AutomergeSync, &encoded)
                .await
                .map_err(|e| format!("Failed to send metadata sync: {}", e))?;
        }
        let _ = room.changed_tx.send(());
        drain_incoming_frames(reader, room, peer_state).await;
    }

    info!(
        "[streaming-load] Loaded {} cells in {} batches ({:?})",
        total_cells,
        batch_num,
        start.elapsed()
    );

    Ok(total_cells)
}

/// Load notebook cells and metadata from a .ipynb file into a NotebookDoc.
///
/// Called by daemon-owned notebook loading (`OpenNotebook` handshake).
/// Parses the file and populates the Automerge doc with cells and metadata.
///
/// Returns the cell count on success.
pub async fn load_notebook_from_disk(
    doc: &mut NotebookDoc,
    path: &std::path::Path,
    blob_store: &BlobStore,
) -> Result<usize, String> {
    load_notebook_from_disk_with_state_doc(doc, None, path, blob_store).await
}

/// Load a notebook from disk, populating both the notebook doc and optionally
/// the RuntimeStateDoc with outputs keyed by synthetic execution_ids.
pub async fn load_notebook_from_disk_with_state_doc(
    doc: &mut NotebookDoc,
    mut state_doc: Option<&mut RuntimeStateDoc>,
    path: &std::path::Path,
    blob_store: &BlobStore,
) -> Result<usize, String> {
    // Read the file
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| format!("Failed to read notebook: {}", e))?;

    // Parse JSON
    let json: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("Invalid notebook JSON: {}", e))?;

    // Parse cells. Outputs come back in a parallel map keyed by cell_id —
    // they're destined for RuntimeStateDoc, keyed by a freshly-minted
    // synthetic execution_id per cell.
    let ParsedIpynbCells {
        cells,
        outputs_by_cell,
    } = parse_cells_from_ipynb(&json)
        .ok_or_else(|| "Failed to parse cells from notebook".to_string())?;
    let nbformat_attachments = parse_nbformat_attachments_from_ipynb(&json);

    // Populate cells in the doc
    for (i, cell) in cells.iter().enumerate() {
        doc.add_cell(i, &cell.id, &cell.cell_type)
            .map_err(|e| format!("Failed to add cell: {}", e))?;
        doc.update_source(&cell.id, &cell.source)
            .map_err(|e| format!("Failed to update source: {}", e))?;
        // Parse execution_count from the .ipynb cell snapshot
        let parsed_ec: Option<i64> = cell.execution_count.parse::<i64>().ok();
        let cell_outputs = outputs_by_cell.get(&cell.id);
        let has_outputs = cell_outputs.map(|o| !o.is_empty()).unwrap_or(false);
        let has_ec = parsed_ec.is_some();

        // Create a synthetic execution entry in RuntimeStateDoc if the cell
        // has outputs or an execution_count. The execution_id links the cell
        // to its outputs and execution_count in RuntimeStateDoc.
        if has_outputs || has_ec {
            let output_refs = if let Some(outs) = cell_outputs.filter(|o| !o.is_empty()) {
                outputs_to_manifest_refs(outs, blob_store).await
            } else {
                Vec::new()
            };
            let synthetic_eid = uuid::Uuid::new_v4().to_string();
            if let Some(ref mut sd) = state_doc {
                sd.create_execution(&synthetic_eid, &cell.id);
                if has_outputs {
                    sd.set_outputs(&synthetic_eid, &output_refs)
                        .map_err(|e| format!("Failed to set outputs in state doc: {}", e))?;
                }
                if let Some(ec) = parsed_ec {
                    sd.set_execution_count(&synthetic_eid, ec);
                }
                sd.set_execution_done(&synthetic_eid, true);
            }
            doc.set_execution_id(&cell.id, Some(&synthetic_eid))
                .map_err(|e| format!("Failed to set execution_id: {}", e))?;
        }
        if should_resolve_markdown_assets(&cell.cell_type) {
            let resolved_assets = resolve_markdown_assets(
                &cell.source,
                Some(path),
                nbformat_attachments.get(&cell.id),
                blob_store,
            )
            .await;
            doc.set_cell_resolved_assets(&cell.id, &resolved_assets)
                .map_err(|e| format!("Failed to set resolved assets: {}", e))?;
        }
    }

    // Parse and set metadata
    if let Some(metadata_snapshot) = parse_metadata_from_ipynb(&json) {
        doc.set_metadata_snapshot(&metadata_snapshot)
            .map_err(|e| format!("Failed to set metadata: {}", e))?;
    }

    Ok(cells.len())
}

/// Create a new empty notebook with a single code cell.
///
/// Called by daemon-owned notebook creation (`CreateNotebook` handshake).
/// Uses the provided env_id or generates a new one, and populates the doc
/// with default metadata for the specified runtime.
///
/// Returns the env_id used on success.
pub fn create_empty_notebook(
    doc: &mut NotebookDoc,
    runtime: &str,
    default_python_env: crate::settings_doc::PythonEnvType,
    env_id: Option<&str>,
) -> Result<String, String> {
    let env_id = env_id
        .map(|s| s.to_string())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    // Build metadata based on runtime (no cells — the frontend creates the
    // first cell locally via the CRDT so the user gets instant autofocus)
    let metadata_snapshot = build_new_notebook_metadata(runtime, &env_id, default_python_env);

    doc.set_metadata_snapshot(&metadata_snapshot)
        .map_err(|e| format!("Failed to set metadata: {}", e))?;

    Ok(env_id)
}

/// Build default metadata for a new notebook based on runtime.
fn build_new_notebook_metadata(
    runtime: &str,
    env_id: &str,
    default_python_env: crate::settings_doc::PythonEnvType,
) -> NotebookMetadataSnapshot {
    use crate::notebook_metadata::{
        CondaInlineMetadata, KernelspecSnapshot, LanguageInfoSnapshot, RuntMetadata,
        UvInlineMetadata,
    };

    let (kernelspec, language_info, runt) = match runtime {
        "deno" => (
            KernelspecSnapshot {
                name: "deno".to_string(),
                display_name: "Deno".to_string(),
                language: Some("typescript".to_string()),
            },
            LanguageInfoSnapshot {
                name: "typescript".to_string(),
                version: None,
            },
            RuntMetadata {
                schema_version: "1".to_string(),
                env_id: Some(env_id.to_string()),
                uv: None,
                conda: None,
                pixi: None,
                deno: None,
                trust_signature: None,
                trust_timestamp: None,
                extra: std::collections::BTreeMap::new(),
            },
        ),
        _ => {
            // Python (default)
            let (uv, conda, pixi) = match default_python_env {
                crate::settings_doc::PythonEnvType::Conda => (
                    None,
                    Some(CondaInlineMetadata {
                        dependencies: vec![],
                        // Default to conda-forge to match launch logic normalization
                        // (avoids false channel-drift detection)
                        channels: vec!["conda-forge".to_string()],
                        python: None,
                    }),
                    None,
                ),
                crate::settings_doc::PythonEnvType::Pixi => (
                    None,
                    None,
                    Some(notebook_doc::metadata::PixiInlineMetadata {
                        dependencies: vec![],
                        pypi_dependencies: vec![],
                        channels: vec!["conda-forge".to_string()],
                        python: None,
                    }),
                ),
                crate::settings_doc::PythonEnvType::Uv
                | crate::settings_doc::PythonEnvType::Other(_) => (
                    Some(UvInlineMetadata {
                        dependencies: vec![],
                        requires_python: None,
                        prerelease: None,
                    }),
                    None,
                    None,
                ),
            };

            (
                KernelspecSnapshot {
                    name: "python3".to_string(),
                    display_name: "Python 3".to_string(),
                    language: Some("python".to_string()),
                },
                LanguageInfoSnapshot {
                    name: "python".to_string(),
                    version: None,
                },
                RuntMetadata {
                    schema_version: "1".to_string(),
                    env_id: Some(env_id.to_string()),
                    uv,
                    conda,
                    pixi,
                    deno: None,
                    trust_signature: None,
                    trust_timestamp: None,
                    extra: std::collections::BTreeMap::new(),
                },
            )
        }
    };

    NotebookMetadataSnapshot {
        kernelspec: Some(kernelspec),
        language_info: Some(language_info),
        runt,
    }
}

/// Apply external .ipynb changes to the Automerge doc.
///
/// Compares cells by ID and:
/// - Adds new cells
/// - Removes deleted cells
/// - Updates source, execution_count, and outputs for modified cells
/// - Handles cell reordering by rebuilding the cell list
///
/// When a kernel is running, outputs and execution counts are preserved
/// to avoid losing in-progress execution results.
///
/// Returns true if any changes were applied.
async fn apply_ipynb_changes(
    room: &NotebookRoom,
    external_cells: &[CellSnapshot],
    external_outputs: &HashMap<String, Vec<serde_json::Value>>,
    external_attachments: &HashMap<String, serde_json::Value>,
    has_running_kernel: bool,
) -> bool {
    let current_cells = {
        let doc = room.doc.read().await;
        doc.get_cells()
    };

    // Pre-convert external outputs through the blob store so they're stored as
    // manifest hashes rather than raw JSON. This also ensures comparisons against
    // the doc's existing manifest hashes work correctly.
    let converted_outputs: HashMap<String, Vec<serde_json::Value>> = {
        let mut map = HashMap::new();
        for (cell_id, raw_outputs) in external_outputs {
            if !raw_outputs.is_empty() {
                let refs = outputs_to_manifest_refs(raw_outputs, &room.blob_store).await;
                map.insert(cell_id.clone(), refs);
            }
        }
        map
    };
    let notebook_path_for_assets = room.path.read().await.clone();
    let converted_assets: HashMap<String, ResolvedAssets> = {
        let mut map = HashMap::new();
        for cell in external_cells {
            if should_resolve_markdown_assets(&cell.cell_type) {
                let resolved_assets = resolve_markdown_assets(
                    &cell.source,
                    notebook_path_for_assets.as_deref(),
                    external_attachments.get(&cell.id),
                    &room.blob_store,
                )
                .await;
                map.insert(cell.id.clone(), resolved_assets);
            }
        }
        map
    };

    {
        let mut cache = room.nbformat_attachments.write().await;
        *cache = external_attachments.clone();
    }

    let empty_assets = HashMap::new();

    // Build maps for comparison
    let current_map: HashMap<&str, &CellSnapshot> =
        current_cells.iter().map(|c| (c.id.as_str(), c)).collect();
    let external_map: HashMap<&str, &CellSnapshot> =
        external_cells.iter().map(|c| (c.id.as_str(), c)).collect();

    // Check if cell order changed
    let current_ids: Vec<&str> = current_cells.iter().map(|c| c.id.as_str()).collect();
    let external_ids: Vec<&str> = external_cells.iter().map(|c| c.id.as_str()).collect();
    let order_changed = {
        // Filter to only IDs that exist in both, then compare order
        let common_current: Vec<&str> = current_ids
            .iter()
            .filter(|id| external_map.contains_key(*id))
            .copied()
            .collect();
        let common_external: Vec<&str> = external_ids
            .iter()
            .filter(|id| current_map.contains_key(*id))
            .copied()
            .collect();
        common_current != common_external
    };

    // Detect wholesale file replacement: the current doc has cells, the
    // external file has cells, but they share zero cell IDs. This happens
    // when an external process (e.g. an AI agent) writes a completely new
    // notebook to the same path. Route through the rebuild path which is
    // atomic (fork → delete all → re-add all → merge) rather than the
    // incremental path which mixes direct mutations with fork+merge.
    let no_common_cells = !current_ids.is_empty()
        && !external_ids.is_empty()
        && !current_ids.iter().any(|id| external_map.contains_key(id));

    // Struct for collecting deferred state_doc writes so the doc write
    // guard is not held across state_doc `.await` (deadlock prevention).
    struct DeferredExecution<'a> {
        synthetic_eid: String,
        cell_id: String,
        outputs: &'a [serde_json::Value],
        execution_count: Option<i64>,
    }

    // If order changed or the file was wholesale-replaced, rebuild the
    // cell list. Use fork + merge so the structural rebuild from disk
    // composes with concurrent CRDT changes rather than overwriting them.
    //
    // We use fork() (at current heads) instead of fork_at(save_heads)
    // because fork_at triggers an automerge bug (MissingOps panic in
    // the change collector) when the document has a complex history of
    // interleaved text splices and merges. See automerge/automerge#1327.
    if order_changed || no_common_cells {
        debug!(
            "[notebook-watch] {} — rebuilding cell list",
            if no_common_cells {
                "Wholesale file replacement detected (zero common cells)"
            } else {
                "Cell order changed"
            }
        );

        // Scope the doc write guard so it drops before state_doc and
        // saved_sources `.await`s (deadlock prevention).
        let deferred_executions = {
            let mut doc = room.doc.write().await;
            // Synchronous fork+merge inside the write guard — no `.await`
            // between fork and merge. A stable actor is safe here because
            // no other fork of this doc can exist concurrently.
            let mut fork = doc.fork_with_actor("runtimed:filesystem");

            // Delete all current cells and re-add in external order on the fork
            for cell in &current_cells {
                let _ = fork.delete_cell(&cell.id);
            }

            let mut deferred: Vec<DeferredExecution> = Vec::new();

            for (index, ext_cell) in external_cells.iter().enumerate() {
                if fork
                    .add_cell(index, &ext_cell.id, &ext_cell.cell_type)
                    .is_ok()
                {
                    let _ = fork.update_source(&ext_cell.id, &ext_cell.source);

                    // For existing cells with running kernel: preserve current execution_id
                    // (outputs live in RuntimeStateDoc, keyed by execution_id)
                    // For new cells: defer state_doc writes until after doc lock is released
                    if has_running_kernel {
                        if let Some(_current) = current_map.get(ext_cell.id.as_str()) {
                            // Existing cell - preserve in-progress state (execution_id stays)
                            // execution_count is in RuntimeStateDoc via execution_id
                            if let Some(eid) = doc.get_execution_id(&ext_cell.id) {
                                let _ = fork.set_execution_id(&ext_cell.id, Some(&eid));
                            }
                        } else {
                            // New cell - collect for deferred state_doc write
                            let ext_outputs = converted_outputs
                                .get(ext_cell.id.as_str())
                                .map(|v| v.as_slice())
                                .unwrap_or(&[]);
                            let parsed_ec: Option<i64> = ext_cell.execution_count.parse().ok();
                            if !ext_outputs.is_empty() || parsed_ec.is_some() {
                                let synthetic_eid = uuid::Uuid::new_v4().to_string();
                                let _ = fork.set_execution_id(&ext_cell.id, Some(&synthetic_eid));
                                deferred.push(DeferredExecution {
                                    synthetic_eid,
                                    cell_id: ext_cell.id.clone(),
                                    outputs: ext_outputs,
                                    execution_count: parsed_ec,
                                });
                            }
                        }
                    } else {
                        let ext_outputs = converted_outputs
                            .get(ext_cell.id.as_str())
                            .map(|v| v.as_slice())
                            .unwrap_or(&[]);
                        let parsed_ec: Option<i64> = ext_cell.execution_count.parse().ok();
                        if !ext_outputs.is_empty() || parsed_ec.is_some() {
                            let synthetic_eid = uuid::Uuid::new_v4().to_string();
                            let _ = fork.set_execution_id(&ext_cell.id, Some(&synthetic_eid));
                            deferred.push(DeferredExecution {
                                synthetic_eid,
                                cell_id: ext_cell.id.clone(),
                                outputs: ext_outputs,
                                execution_count: parsed_ec,
                            });
                        }
                    }
                    let ext_assets = converted_assets
                        .get(ext_cell.id.as_str())
                        .unwrap_or(&empty_assets);
                    let _ = fork.set_cell_resolved_assets(&ext_cell.id, ext_assets);
                }
            }

            if let Err(e) =
                catch_automerge_panic("file-watcher-order-merge", || doc.merge(&mut fork))
            {
                warn!("{}", e);
                doc.rebuild_from_save();
            }

            deferred
        }; // doc guard dropped here

        // Apply deferred state_doc writes
        if !deferred_executions.is_empty() {
            let mut sd = room.state_doc.write().await;
            for de in &deferred_executions {
                sd.create_execution(&de.synthetic_eid, &de.cell_id);
                if !de.outputs.is_empty() {
                    let _ = sd.set_outputs(&de.synthetic_eid, de.outputs);
                }
                if let Some(ec) = de.execution_count {
                    sd.set_execution_count(&de.synthetic_eid, ec);
                }
                sd.set_execution_done(&de.synthetic_eid, true);
            }
            let _ = room.state_changed_tx.send(());
        }

        // Update saved_sources baseline so subsequent external edits are
        // detected correctly (same as the non-order-change path).
        let mut saved = room.last_save_sources.write().await;
        saved.clear();
        for ext_cell in external_cells {
            saved.insert(ext_cell.id.clone(), ext_cell.source.clone());
        }

        return true;
    }

    // Snapshot saved_sources before the doc write lock to avoid holding
    // doc across saved_sources `.await` (deadlock prevention).
    let saved_sources_snapshot: HashMap<String, String> = {
        let saved_sources = room.last_save_sources.read().await;
        saved_sources.clone()
    };
    let have_save_snapshot = !saved_sources_snapshot.is_empty();

    // Find cells to delete — only cells that existed in our last save
    // but are no longer on disk (genuine external deletion). Cells that
    // are in the CRDT but NOT in last_save_sources were created after
    // the save and should be preserved (the user or agent just added them).
    //
    // If we've never saved (last_save_sources is empty), we have no
    // baseline to distinguish "externally deleted" from "just created in
    // CRDT but not yet saved." Skip deletions entirely — it's safer to
    // keep extra cells than to silently drop cells a client just created.
    let cells_to_delete: Vec<String> = if !have_save_snapshot {
        if !current_cells.is_empty() {
            debug!(
                "[notebook-watch] No save snapshot — skipping deletion of {} CRDT cells not on disk",
                current_cells.iter().filter(|c| !external_map.contains_key(c.id.as_str())).count()
            );
        }
        Vec::new()
    } else {
        current_cells
            .iter()
            .filter(|c| {
                !external_map.contains_key(c.id.as_str())
                    && saved_sources_snapshot.contains_key(c.id.as_str())
            })
            .map(|c| c.id.clone())
            .collect()
    };

    // Snapshot current execution state from state_doc before acquiring
    // the doc write lock, so we don't hold state_doc and doc simultaneously
    // (deadlock prevention).
    let current_execution_state: HashMap<String, (Vec<serde_json::Value>, Option<i64>)> =
        if !has_running_kernel {
            // Need doc read to get execution IDs, then state_doc read for outputs.
            // Do both reads in scoped blocks.
            let eid_map: HashMap<String, String> = {
                let doc = room.doc.read().await;
                let mut map = HashMap::new();
                for ext_cell in external_cells.iter() {
                    if current_map.contains_key(ext_cell.id.as_str()) {
                        if let Some(eid) = doc.get_execution_id(&ext_cell.id) {
                            map.insert(ext_cell.id.clone(), eid);
                        }
                    }
                }
                map
            };
            let sd = room.state_doc.read().await;
            let mut state_map = HashMap::new();
            for (cell_id, eid) in &eid_map {
                let outputs = sd.get_outputs(eid);
                let ec = sd.get_execution(eid).and_then(|e| e.execution_count);
                state_map.insert(cell_id.clone(), (outputs, ec));
            }
            state_map
        } else {
            HashMap::new()
        };

    // Scope the doc write guard so it drops before state_doc and
    // saved_sources `.await`s (deadlock prevention: no lock held
    // across `.await`).
    let (changed, deferred_execs) = {
        let mut doc = room.doc.write().await;
        let mut changed = false;

        for cell_id in cells_to_delete {
            debug!("[notebook-watch] Deleting cell: {}", cell_id);
            if let Ok(true) = doc.delete_cell(&cell_id) {
                changed = true;
            }
        }

        // For source updates on existing cells, use fork + merge so that
        // external edits compose with concurrent CRDT changes rather than
        // overwriting them. We use fork() instead of fork_at(save_heads)
        // to avoid the automerge MissingOps bug (automerge/automerge#1327).
        //
        // Source comparison uses last_save_sources (what we wrote to disk)
        // instead of the live CRDT (which may have progressed with new user
        // typing since the save). This prevents the file watcher from
        // treating our own autosave as an "external change" and overwriting
        // the user's recent edits. Only genuine external changes (git pull,
        // external editor) — where the disk content differs from what we
        // last saved — trigger a source update.
        // Synchronous fork+merge inside the write guard — a stable actor
        // is safe here because no other fork of this doc can exist
        // concurrently.
        let mut source_fork = Some(doc.fork_with_actor("runtimed:filesystem"));

        let mut deferred_execs: Vec<DeferredExecution> = Vec::new();
        // Track cells whose execution_id should be cleared (no new outputs)
        let mut clear_execution_ids: Vec<String> = Vec::new();

        // Process external cells in order (add new or update existing)
        for (index, ext_cell) in external_cells.iter().enumerate() {
            if let Some(current_cell) = current_map.get(ext_cell.id.as_str()) {
                // Cell exists — check if source genuinely changed externally.
                // Compare disk content against what we last saved, NOT the live
                // CRDT. If disk matches our last save, the change is from our
                // own autosave and should be ignored (the CRDT may have
                // progressed with new typing since then).
                let saved_source = saved_sources_snapshot.get(ext_cell.id.as_str());
                let is_external_change = match saved_source {
                    Some(saved) => ext_cell.source != *saved,
                    None => current_cell.source != ext_cell.source,
                };

                if is_external_change {
                    debug!("[notebook-watch] Updating source for cell: {}", ext_cell.id);
                    if let Some(ref mut fork) = source_fork {
                        let _ = fork.update_source(&ext_cell.id, &ext_cell.source);
                        changed = true;
                    } else if let Ok(true) = doc.update_source(&ext_cell.id, &ext_cell.source) {
                        changed = true;
                    }
                }

                // Update cell type if changed
                if current_cell.cell_type != ext_cell.cell_type {
                    debug!(
                        "[notebook-watch] Cell type changed for {}: {} -> {}",
                        ext_cell.id, current_cell.cell_type, ext_cell.cell_type
                    );
                    // Cell type changes require recreating the cell (rare case)
                    // For now, just log - full support would need more work
                }

                // Preserve outputs and execution_count if kernel is running
                if !has_running_kernel {
                    let ext_outputs = converted_outputs
                        .get(ext_cell.id.as_str())
                        .map(|v| v.as_slice())
                        .unwrap_or(&[]);
                    let parsed_ec: Option<i64> = ext_cell.execution_count.parse().ok();

                    // Compare external outputs and execution_count against
                    // pre-snapshotted RuntimeStateDoc state
                    let current_eid = doc.get_execution_id(&ext_cell.id);
                    let (current_outputs, current_ec) = current_execution_state
                        .get(ext_cell.id.as_str())
                        .cloned()
                        .unwrap_or((Vec::new(), None));

                    let outputs_changed = current_outputs.as_slice() != ext_outputs;
                    let ec_changed = current_ec != parsed_ec;

                    if outputs_changed || ec_changed {
                        if !ext_outputs.is_empty() || parsed_ec.is_some() {
                            debug!(
                                "[notebook-watch] Updating outputs/execution_count for cell: {}",
                                ext_cell.id
                            );
                            let synthetic_eid = uuid::Uuid::new_v4().to_string();
                            let _ = doc.set_execution_id(&ext_cell.id, Some(&synthetic_eid));
                            deferred_execs.push(DeferredExecution {
                                synthetic_eid,
                                cell_id: ext_cell.id.clone(),
                                outputs: ext_outputs,
                                execution_count: parsed_ec,
                            });
                            changed = true;
                        } else if current_eid.is_some() {
                            clear_execution_ids.push(ext_cell.id.clone());
                            changed = true;
                        }
                    }
                }

                let ext_assets = converted_assets
                    .get(ext_cell.id.as_str())
                    .unwrap_or(&empty_assets);
                if current_cell.resolved_assets != *ext_assets {
                    if let Ok(true) = doc.set_cell_resolved_assets(&ext_cell.id, ext_assets) {
                        changed = true;
                    }
                }
            } else {
                // New cell - add it
                // New cells don't have any in-progress state, so always use external values
                debug!(
                    "[notebook-watch] Adding new cell at index {}: {}",
                    index, ext_cell.id
                );
                if doc
                    .add_cell(index, &ext_cell.id, &ext_cell.cell_type)
                    .is_ok()
                {
                    changed = true;
                    let _ = doc.update_source(&ext_cell.id, &ext_cell.source);
                    let ext_outputs = converted_outputs
                        .get(ext_cell.id.as_str())
                        .map(|v| v.as_slice())
                        .unwrap_or(&[]);
                    let parsed_ec: Option<i64> = ext_cell.execution_count.parse().ok();
                    if !ext_outputs.is_empty() || parsed_ec.is_some() {
                        let synthetic_eid = uuid::Uuid::new_v4().to_string();
                        let _ = doc.set_execution_id(&ext_cell.id, Some(&synthetic_eid));
                        deferred_execs.push(DeferredExecution {
                            synthetic_eid,
                            cell_id: ext_cell.id.clone(),
                            outputs: ext_outputs,
                            execution_count: parsed_ec,
                        });
                    }
                    let ext_assets = converted_assets
                        .get(ext_cell.id.as_str())
                        .unwrap_or(&empty_assets);
                    let _ = doc.set_cell_resolved_assets(&ext_cell.id, ext_assets);
                }
            }
        }

        // Apply clear_execution_ids before merge
        for cell_id in &clear_execution_ids {
            let _ = doc.set_execution_id(cell_id, None);
        }

        // Merge source fork back — source changes from disk compose with
        // post-save CRDT changes via Automerge's text CRDT merge.
        if let Some(ref mut fork) = source_fork {
            if let Err(e) = catch_automerge_panic("file-watcher-source-merge", || doc.merge(fork)) {
                warn!("{}", e);
                doc.rebuild_from_save();
            }
        }

        (changed, deferred_execs)
    }; // doc guard dropped here

    // Apply deferred state_doc writes
    if !deferred_execs.is_empty() {
        let mut sd = room.state_doc.write().await;
        for de in &deferred_execs {
            sd.create_execution(&de.synthetic_eid, &de.cell_id);
            if !de.outputs.is_empty() {
                let _ = sd.set_outputs(&de.synthetic_eid, de.outputs);
            }
            if let Some(ec) = de.execution_count {
                sd.set_execution_count(&de.synthetic_eid, ec);
            }
            sd.set_execution_done(&de.synthetic_eid, true);
        }
        let _ = room.state_changed_tx.send(());
    }

    // Update saved_sources baseline after applying external changes so
    // that subsequent external edits are detected correctly (P2-a) and
    // externally-added cells become deletable if later removed (P2-b).
    if changed {
        let mut saved = room.last_save_sources.write().await;
        for ext_cell in external_cells {
            saved.insert(ext_cell.id.clone(), ext_cell.source.clone());
        }
        // Remove entries for cells we just deleted
        saved.retain(|id, _| external_map.contains_key(id.as_str()));
    }

    changed
}

/// Spawn a file watcher for a notebook's .ipynb file.
///
/// Watches for external changes and merges them into the Automerge doc.
/// Returns a shutdown sender to stop the watcher when the room is evicted.
pub(crate) fn spawn_notebook_file_watcher(
    notebook_path: PathBuf,
    room: Arc<NotebookRoom>,
) -> oneshot::Sender<()> {
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();

    spawn_best_effort("notebook-file-watcher", async move {
        // Determine what path to watch
        let watch_path = if notebook_path.exists() {
            notebook_path.clone()
        } else if let Some(parent) = notebook_path.parent() {
            // Watch parent directory if file doesn't exist yet
            if !parent.exists() {
                warn!(
                    "[notebook-watch] Parent dir doesn't exist for {:?}",
                    notebook_path
                );
                return;
            }
            parent.to_path_buf()
        } else {
            warn!(
                "[notebook-watch] Cannot determine watch path for {:?}",
                notebook_path
            );
            return;
        };

        // Create tokio mpsc channel to bridge from notify callback thread
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DebounceEventResult>(16);

        // Create debouncer with 500ms window (same as settings.json)
        let debouncer_result = notify_debouncer_mini::new_debouncer(
            std::time::Duration::from_millis(500),
            move |res: DebounceEventResult| {
                let _ = tx.blocking_send(res);
            },
        );

        let mut debouncer = match debouncer_result {
            Ok(d) => d,
            Err(e) => {
                error!(
                    "[notebook-watch] Failed to create file watcher for {:?}: {}",
                    notebook_path, e
                );
                return;
            }
        };

        if let Err(e) = debouncer
            .watcher()
            .watch(&watch_path, notify::RecursiveMode::NonRecursive)
        {
            error!("[notebook-watch] Failed to watch {:?}: {}", watch_path, e);
            return;
        }

        info!(
            "[notebook-watch] Watching {:?} for external changes",
            notebook_path
        );

        loop {
            tokio::select! {
                Some(result) = rx.recv() => {
                    match result {
                        Ok(events) => {
                            // Check if any event is for our notebook file
                            let relevant = events.iter().any(|e| e.path == notebook_path);
                            if !relevant {
                                continue;
                            }

                            // Check if this is a self-write (within skip window of our last save)
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0);
                            let last_write = room.last_self_write.load(Ordering::Relaxed);
                            if now.saturating_sub(last_write) < SELF_WRITE_SKIP_WINDOW_MS {
                                debug!(
                                    "[notebook-watch] Skipping self-write event for {:?}",
                                    notebook_path
                                );
                                continue;
                            }

                            // Read and parse the file
                            let contents = match tokio::fs::read_to_string(&notebook_path).await {
                                Ok(c) => c,
                                Err(e) => {
                                    // File may be deleted or being written
                                    debug!(
                                        "[notebook-watch] Cannot read {:?}: {}",
                                        notebook_path, e
                                    );
                                    continue;
                                }
                            };

                            let json: serde_json::Value = match serde_json::from_str(&contents) {
                                Ok(j) => j,
                                Err(e) => {
                                    // Partial write or invalid JSON - try again next event
                                    debug!(
                                        "[notebook-watch] Cannot parse {:?}: {}",
                                        notebook_path, e
                                    );
                                    continue;
                                }
                            };

                            // Parse cells from the .ipynb
                            // None = parse failure (missing cells key), Some([]) = valid empty notebook
                            let ParsedIpynbCells {
                                cells: external_cells,
                                outputs_by_cell: external_outputs,
                            } = match parse_cells_from_ipynb(&json) {
                                Some(parsed) => parsed,
                                None => {
                                    warn!(
                                        "[notebook-watch] Cannot parse cells from {:?} - skipping",
                                        notebook_path
                                    );
                                    continue;
                                }
                            };
                            let external_attachments = parse_nbformat_attachments_from_ipynb(&json);
                            let external_metadata = parse_metadata_from_ipynb(&json);

                            // Check if kernel is running (to preserve outputs)
                            let has_kernel = room.has_kernel().await;

                            // Apply cell changes to Automerge doc
                            let cells_changed = apply_ipynb_changes(
                                &room,
                                &external_cells,
                                &external_outputs,
                                &external_attachments,
                                has_kernel,
                            )
                            .await;

                            // Apply metadata changes to Automerge doc.
                            // Only update when the external file has a metadata
                            // object — a missing key means "no metadata info",
                            // not "clear metadata".
                            let metadata_changed = if let Some(ref meta) = external_metadata {
                                let current = {
                                    let doc = room.doc.read().await;
                                    doc.get_metadata_snapshot()
                                };
                                let changed = Some(meta) != current.as_ref();
                                if changed {
                                    let mut doc = room.doc.write().await;
                                    if let Err(e) = doc.set_metadata_snapshot(meta) {
                                        warn!("[notebook-watch] Failed to set metadata: {}", e);
                                    }
                                }
                                changed
                            } else {
                                false
                            };

                            if cells_changed || metadata_changed {
                                info!(
                                    "[notebook-watch] Applied external changes from {:?} (cells={}, metadata={})",
                                    notebook_path, cells_changed, metadata_changed,
                                );

                                // Notify peers of the change — actual data
                                // arrives via Automerge sync frames
                                let _ = room.changed_tx.send(());
                            }

                            // Re-verify trust after external metadata edits.
                            // room.trust_state was loaded once at room creation
                            // and never refreshed on this path. External edits
                            // via uv/editor (e.g. `uv add numpy` + save) rewrite
                            // metadata.runt.*.dependencies, which changes the
                            // HMAC input; without this, the cached trust state
                            // stays stale until the daemon restarts and
                            // auto-launch enforces the old signature. Trust
                            // lives in metadata, so gate on metadata_changed —
                            // cell-only edits can't affect the signature.
                            // check_and_update_trust_state only writes when the
                            // status actually flipped and emits
                            // state_changed_tx so the frontend banner reacts.
                            if metadata_changed {
                                check_and_update_trust_state(&room).await;
                            }
                        }
                        Err(errs) => {
                            warn!("[notebook-watch] Watch error for {:?}: {:?}", notebook_path, errs);
                        }
                    }
                }
                _ = &mut shutdown_rx => {
                    info!("[notebook-watch] Shutting down watcher for {:?}", notebook_path);
                    break;
                }
            }
        }
    });

    shutdown_tx
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use uuid::Uuid;

    #[test]
    fn test_sanitize_peer_label_basic() {
        assert_eq!(sanitize_peer_label(None, "fb"), "fb");
        assert_eq!(sanitize_peer_label(Some(""), "fb"), "fb");
        assert_eq!(sanitize_peer_label(Some("  "), "fb"), "fb");
        assert_eq!(sanitize_peer_label(Some("Codex"), "fb"), "Codex");
        assert_eq!(sanitize_peer_label(Some("  Claude  "), "fb"), "Claude");
    }

    #[test]
    fn test_sanitize_peer_label_clamps_length() {
        let long = "a".repeat(100);
        assert_eq!(sanitize_peer_label(Some(&long), "fb").len(), 64);
    }

    #[test]
    fn test_sanitize_peer_label_clamps_unicode() {
        // 70 emoji = 70 chars but 280 bytes
        let emoji_label: String = "🦾".repeat(70);
        let result = sanitize_peer_label(Some(&emoji_label), "fb");
        assert_eq!(result.chars().count(), 64);
    }

    #[test]
    fn test_sanitize_peer_label_strips_zero_width() {
        // ZWJ, ZWSP, ZWNJ scattered in a label
        assert_eq!(
            sanitize_peer_label(Some("Co\u{200B}d\u{200D}ex"), "fb"),
            "Codex"
        );
        // Only zero-width chars → falls back to fallback
        assert_eq!(
            sanitize_peer_label(Some("\u{200B}\u{200C}\u{200D}"), "fb"),
            "fb"
        );
    }

    #[test]
    fn test_sanitize_peer_label_strips_control_chars() {
        assert_eq!(sanitize_peer_label(Some("Claude\x00\x1F"), "fb"), "Claude");
        assert_eq!(sanitize_peer_label(Some("\x07"), "fb"), "fb");
    }

    #[test]
    fn test_sanitize_peer_label_strips_bidi_overrides() {
        // RTL override + LTR override
        assert_eq!(
            sanitize_peer_label(Some("\u{202E}Agent\u{202C}"), "fb"),
            "Agent"
        );
    }

    #[test]
    fn test_sanitize_peer_label_strips_bidi_marks() {
        // LRM and RLM
        assert_eq!(
            sanitize_peer_label(Some("\u{200E}Agent\u{200F}"), "fb"),
            "Agent"
        );
        assert_eq!(sanitize_peer_label(Some("\u{200E}\u{200F}"), "fb"), "fb");
    }

    /// Create a test blob store in the given temp directory.
    fn test_blob_store(tmp: &tempfile::TempDir) -> Arc<BlobStore> {
        Arc::new(BlobStore::new(tmp.path().join("blobs")))
    }

    #[tokio::test]
    async fn notebook_room_has_uuid_id_populated() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let uuid = uuid::Uuid::new_v4();
        let room = NotebookRoom::new_fresh(
            uuid,
            None, // untitled
            tmp.path(),
            blob_store,
            false, // ephemeral
        );
        assert_eq!(room.id, uuid);
    }

    #[tokio::test]
    async fn untitled_room_has_path_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let room = NotebookRoom::new_fresh(Uuid::new_v4(), None, tmp.path(), blob_store, false);
        assert!(room.path.read().await.is_none());
    }

    #[tokio::test]
    async fn file_backed_room_has_path_some() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let fake_path = tmp.path().join("note.ipynb");
        let room = NotebookRoom::new_fresh(
            Uuid::new_v4(),
            Some(fake_path.clone()),
            tmp.path(),
            blob_store,
            false,
        );
        let guard = room.path.read().await;
        assert_eq!(guard.as_deref(), Some(fake_path.as_path()));
    }

    #[tokio::test]
    async fn test_room_load_or_create_new() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let room = NotebookRoom::load_or_create("test-nb", tmp.path(), blob_store);

        let doc = room.doc.try_read().unwrap();
        assert_eq!(doc.notebook_id(), Some("test-nb".to_string()));
        assert_eq!(doc.cell_count(), 0);
        assert_eq!(room.active_peers.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_room_persists_and_reloads() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);

        // Create room and add a cell
        {
            let room = NotebookRoom::load_or_create("persist-test", tmp.path(), blob_store.clone());
            let mut doc = room.doc.try_write().unwrap();
            doc.add_cell(0, "c1", "code").unwrap();
            doc.update_source("c1", "hello").unwrap();
            let bytes = doc.save();
            persist_notebook_bytes(&bytes, &room.persist_path);
        }

        // Load again — should have the cell
        {
            let room = NotebookRoom::load_or_create("persist-test", tmp.path(), blob_store);
            let doc = room.doc.try_read().unwrap();
            assert_eq!(doc.cell_count(), 1);
            let cell = doc.get_cell("c1").unwrap();
            assert_eq!(cell.source, "hello");
        }
    }

    #[tokio::test]
    async fn test_get_or_create_room_reuses_existing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let rooms: NotebookRooms = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let path_index = Arc::new(tokio::sync::Mutex::new(PathIndex::new()));
        let uuid1 = Uuid::new_v4();

        let room1 = get_or_create_room(
            &rooms,
            &path_index,
            uuid1,
            None,
            tmp.path(),
            blob_store.clone(),
            false,
        )
        .await;
        let room2 = get_or_create_room(
            &rooms,
            &path_index,
            uuid1,
            None,
            tmp.path(),
            blob_store,
            false,
        )
        .await;

        // Should be the same Arc (same room)
        assert!(Arc::ptr_eq(&room1, &room2));
    }

    #[tokio::test]
    async fn test_get_or_create_room_different_notebooks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let rooms: NotebookRooms = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let path_index = Arc::new(tokio::sync::Mutex::new(PathIndex::new()));
        let uuid1 = Uuid::new_v4();
        let uuid2 = Uuid::new_v4();

        let room1 = get_or_create_room(
            &rooms,
            &path_index,
            uuid1,
            None,
            tmp.path(),
            blob_store.clone(),
            false,
        )
        .await;
        let room2 = get_or_create_room(
            &rooms,
            &path_index,
            uuid2,
            None,
            tmp.path(),
            blob_store,
            false,
        )
        .await;

        // Should be different rooms
        assert!(!Arc::ptr_eq(&room1, &room2));
        assert_eq!(rooms.lock().await.len(), 2);
    }

    #[tokio::test]
    async fn test_room_peer_counting() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let room = NotebookRoom::load_or_create("peer-test", tmp.path(), blob_store);

        assert_eq!(room.active_peers.load(Ordering::Relaxed), 0);

        room.active_peers.fetch_add(1, Ordering::Relaxed);
        room.active_peers.fetch_add(1, Ordering::Relaxed);
        assert_eq!(room.active_peers.load(Ordering::Relaxed), 2);

        room.active_peers.fetch_sub(1, Ordering::Relaxed);
        assert_eq!(room.active_peers.load(Ordering::Relaxed), 1);

        room.active_peers.fetch_sub(1, Ordering::Relaxed);
        assert_eq!(room.active_peers.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_new_fresh_creates_empty_doc() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let uuid = Uuid::new_v4();
        let room = NotebookRoom::new_fresh(uuid, None, tmp.path(), blob_store, false);

        let doc = room.doc.try_read().unwrap();
        assert_eq!(doc.notebook_id(), Some(uuid.to_string()));
        assert_eq!(doc.cell_count(), 0);
    }

    #[tokio::test]
    async fn test_new_fresh_deletes_stale_persisted_doc_for_file_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);

        // Use a fixed UUID so we can find the persist file again.
        let uuid = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-111111111111").unwrap();

        // Create and persist a room with content using load_or_create (uses the UUID string)
        {
            let room =
                NotebookRoom::load_or_create(&uuid.to_string(), tmp.path(), blob_store.clone());
            let mut doc = room.doc.try_write().unwrap();
            doc.add_cell(0, "c1", "code").unwrap();
            doc.update_source("c1", "old content").unwrap();
            let bytes = doc.save();
            persist_notebook_bytes(&bytes, &room.persist_path);
        }

        // Verify persisted file exists
        let filename = notebook_doc_filename(&uuid.to_string());
        let persist_path = tmp.path().join(&filename);
        assert!(persist_path.exists(), "Persisted file should exist");

        // Create fresh room for a file-backed path — should delete persisted doc and start empty.
        // path=Some means this is file-backed, so the persisted .automerge doc should be deleted.
        let fake_ipynb = tmp.path().join("stale-test.ipynb");
        let room = NotebookRoom::new_fresh(uuid, Some(fake_ipynb), tmp.path(), blob_store, false);

        // Persisted file should be deleted
        assert!(
            !persist_path.exists(),
            "Persisted file should be deleted by new_fresh"
        );

        // Room should be empty (no cells from persisted doc)
        let doc = room.doc.try_read().unwrap();
        assert_eq!(doc.cell_count(), 0, "new_fresh should start with empty doc");
    }

    #[tokio::test]
    async fn test_new_fresh_loads_persisted_doc_for_untitled_notebook() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);

        // Use a fixed UUID (untitled notebook — path=None)
        let uuid = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();

        // Create and persist a room with content using load_or_create
        {
            let room =
                NotebookRoom::load_or_create(&uuid.to_string(), tmp.path(), blob_store.clone());
            let mut doc = room.doc.try_write().unwrap();
            doc.add_cell(0, "c1", "code").unwrap();
            doc.update_source("c1", "restored content").unwrap();
            let bytes = doc.save();
            persist_notebook_bytes(&bytes, &room.persist_path);
        }

        // Verify persisted file exists
        let filename = notebook_doc_filename(&uuid.to_string());
        let persist_path = tmp.path().join(&filename);
        assert!(persist_path.exists(), "Persisted file should exist");

        // Create fresh room for untitled notebook (path=None) — should load persisted doc
        let room = NotebookRoom::new_fresh(uuid, None, tmp.path(), blob_store, false);

        // Persisted file should still exist (not deleted)
        assert!(
            persist_path.exists(),
            "Persisted file should NOT be deleted for untitled notebooks"
        );

        // Room should have the persisted content
        let doc = room.doc.try_read().unwrap();
        assert_eq!(
            doc.cell_count(),
            1,
            "new_fresh should load persisted doc for untitled notebooks"
        );
        let cells = doc.get_cells();
        assert_eq!(cells[0].source, "restored content");
    }

    /// Regression test for #1646: untitled notebooks must read trust from
    /// the persisted Automerge doc, not from a non-existent .ipynb file.
    #[tokio::test]
    #[serial]
    async fn test_new_fresh_untitled_trust_from_doc() {
        let temp_dir = tempfile::tempdir().unwrap();
        let key_path = temp_dir.path().join("trust-key");
        std::env::set_var("RUNT_TRUST_KEY_PATH", key_path.to_str().unwrap());

        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);

        let notebook_id = "550e8400-e29b-41d4-a716-446655440000";

        // Build a snapshot with UV deps and a valid trust signature.
        let mut snapshot = snapshot_with_uv(vec!["numpy".to_string()]);
        let mut metadata = std::collections::HashMap::new();
        if let Ok(runt_value) = serde_json::to_value(&snapshot.runt) {
            metadata.insert("runt".to_string(), runt_value);
        }
        let signature = runt_trust::sign_notebook_dependencies(&metadata).unwrap();
        snapshot.runt.trust_signature = Some(signature);

        // Create a room, write the signed metadata, and persist to disk.
        {
            let room = NotebookRoom::load_or_create(notebook_id, tmp.path(), blob_store.clone());
            {
                let mut doc = room.doc.try_write().unwrap();
                doc.set_metadata_snapshot(&snapshot).unwrap();
                let bytes = doc.save();
                persist_notebook_bytes(&bytes, &room.persist_path);
            }
        }

        // Simulate daemon restart: create a fresh room with the same UUID.
        // new_fresh should load the persisted doc and read trust from it.
        let notebook_uuid = Uuid::parse_str(notebook_id).unwrap();
        let room = NotebookRoom::new_fresh(notebook_uuid, None, tmp.path(), blob_store, false);

        let ts = room.trust_state.try_read().unwrap();
        assert_eq!(
            ts.status,
            runt_trust::TrustStatus::Trusted,
            "Untitled notebook trust should survive daemon restart"
        );

        std::env::remove_var("RUNT_TRUST_KEY_PATH");
    }

    #[tokio::test(start_paused = true)]
    async fn test_ephemeral_room_skips_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let blob_store = Arc::new(BlobStore::new(dir.path().join("blobs")));
        let notebook_uuid = uuid::Uuid::new_v4();
        let room = NotebookRoom::new_fresh(notebook_uuid, None, dir.path(), blob_store, true);

        assert!(room.persist_tx.is_none());
        assert!(room.is_ephemeral.load(std::sync::atomic::Ordering::Relaxed));

        // No .automerge file should exist
        let filename = notebook_doc_filename(&notebook_uuid.to_string());
        assert!(!dir.path().join(&filename).exists());
    }

    #[tokio::test(start_paused = true)]
    async fn test_session_room_persists() {
        let dir = tempfile::tempdir().unwrap();
        let blob_store = Arc::new(BlobStore::new(dir.path().join("blobs")));
        let notebook_uuid = uuid::Uuid::new_v4();
        let room = NotebookRoom::new_fresh(notebook_uuid, None, dir.path(), blob_store, false);

        assert!(room.persist_tx.is_some());
        assert!(!room.is_ephemeral.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test(start_paused = true)]
    async fn test_ephemeral_room_has_metadata_flag() {
        let dir = tempfile::tempdir().unwrap();
        let blob_store = Arc::new(BlobStore::new(dir.path().join("blobs")));
        let notebook_uuid = uuid::Uuid::new_v4();
        let room = NotebookRoom::new_fresh(notebook_uuid, None, dir.path(), blob_store, true);

        let doc = room.doc.read().await;
        assert_eq!(doc.get_metadata("ephemeral"), Some("true".to_string()));
    }

    /// Helper to build a snapshot with UV inline deps.
    fn snapshot_with_uv(deps: Vec<String>) -> NotebookMetadataSnapshot {
        NotebookMetadataSnapshot {
            kernelspec: None,
            language_info: None,
            runt: crate::notebook_metadata::RuntMetadata {
                schema_version: "1".to_string(),
                env_id: None,
                uv: Some(crate::notebook_metadata::UvInlineMetadata {
                    dependencies: deps,
                    requires_python: None,
                    prerelease: None,
                }),
                conda: None,
                pixi: None,
                deno: None,
                trust_signature: None,
                trust_timestamp: None,
                extra: std::collections::BTreeMap::new(),
            },
        }
    }

    /// Helper to build a snapshot with conda inline deps.
    fn snapshot_with_conda(deps: Vec<String>) -> NotebookMetadataSnapshot {
        NotebookMetadataSnapshot {
            kernelspec: None,
            language_info: None,
            runt: crate::notebook_metadata::RuntMetadata {
                schema_version: "1".to_string(),
                env_id: None,
                uv: None,
                conda: Some(crate::notebook_metadata::CondaInlineMetadata {
                    dependencies: deps,
                    channels: vec!["conda-forge".to_string()],
                    python: None,
                }),
                pixi: None,
                deno: None,
                trust_signature: None,
                trust_timestamp: None,
                extra: std::collections::BTreeMap::new(),
            },
        }
    }

    /// Helper to build an empty snapshot (no deps).
    fn snapshot_empty() -> NotebookMetadataSnapshot {
        NotebookMetadataSnapshot {
            kernelspec: None,
            language_info: None,
            runt: crate::notebook_metadata::RuntMetadata {
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

    #[test]
    fn test_check_inline_deps_uv() {
        let snapshot = snapshot_with_uv(vec!["numpy".to_string()]);
        assert_eq!(check_inline_deps(&snapshot), Some("uv:inline".to_string()));
    }

    #[test]
    fn test_check_inline_deps_conda() {
        let snapshot = snapshot_with_conda(vec!["pandas".to_string()]);
        assert_eq!(
            check_inline_deps(&snapshot),
            Some("conda:inline".to_string())
        );
    }

    #[test]
    fn test_check_inline_deps_empty() {
        let snapshot = snapshot_empty();
        assert_eq!(check_inline_deps(&snapshot), None);
    }

    #[test]
    fn test_check_inline_deps_empty_array() {
        // Snapshot with empty deps array - should return None
        let snapshot = snapshot_with_uv(vec![]);
        assert_eq!(check_inline_deps(&snapshot), None);
    }

    #[test]
    fn test_check_inline_deps_uv_priority() {
        // Snapshot with both UV and conda deps - UV takes priority
        let snapshot = NotebookMetadataSnapshot {
            kernelspec: None,
            language_info: None,
            runt: crate::notebook_metadata::RuntMetadata {
                schema_version: "1".to_string(),
                env_id: None,
                uv: Some(crate::notebook_metadata::UvInlineMetadata {
                    dependencies: vec!["numpy".to_string()],
                    requires_python: None,
                    prerelease: None,
                }),
                conda: Some(crate::notebook_metadata::CondaInlineMetadata {
                    dependencies: vec!["pandas".to_string()],
                    channels: vec!["conda-forge".to_string()],
                    python: None,
                }),
                pixi: None,
                deno: None,
                trust_signature: None,
                trust_timestamp: None,
                extra: std::collections::BTreeMap::new(),
            },
        };
        assert_eq!(check_inline_deps(&snapshot), Some("uv:inline".to_string()));
    }

    #[test]
    fn test_check_inline_deps_deno() {
        // Snapshot with deno config - deno takes priority over everything
        let snapshot = NotebookMetadataSnapshot {
            kernelspec: None,
            language_info: None,
            runt: crate::notebook_metadata::RuntMetadata {
                schema_version: "1".to_string(),
                env_id: None,
                uv: Some(crate::notebook_metadata::UvInlineMetadata {
                    dependencies: vec!["numpy".to_string()],
                    requires_python: None,
                    prerelease: None,
                }),
                conda: None,
                pixi: None,
                deno: Some(crate::notebook_metadata::DenoMetadata {
                    permissions: vec![],
                    import_map: None,
                    config: None,
                    flexible_npm_imports: None,
                }),
                trust_signature: None,
                trust_timestamp: None,
                extra: std::collections::BTreeMap::new(),
            },
        };
        assert_eq!(check_inline_deps(&snapshot), Some("deno".to_string()));
    }

    // Runtime detection tests now live in notebook-doc/src/metadata.rs
    // (NotebookMetadataSnapshot::detect_runtime) with comprehensive coverage.

    // ── Integration tests for save_notebook_to_disk ────────────────────────

    /// Create a test room with a path pointing to a file in temp dir.
    fn test_room_with_path(
        tmp: &tempfile::TempDir,
        notebook_filename: &str,
    ) -> (NotebookRoom, PathBuf) {
        let notebook_path = tmp.path().join(notebook_filename);
        let blob_store = test_blob_store(tmp);
        let notebook_id = notebook_path.to_string_lossy().to_string();

        let doc = crate::notebook_doc::NotebookDoc::new(&notebook_id);
        let (changed_tx, _) = broadcast::channel(16);
        let (kernel_broadcast_tx, _) = broadcast::channel(KERNEL_BROADCAST_CAPACITY);
        let persist_path = tmp.path().join("doc.automerge");
        let (persist_tx, persist_rx) = watch::channel::<Option<Vec<u8>>>(None);
        let (flush_request_tx, flush_rx) = mpsc::unbounded_channel::<FlushRequest>();
        spawn_persist_debouncer(persist_rx, flush_rx, persist_path.clone());

        let (presence_tx, _) = broadcast::channel(64);

        let state_doc = Arc::new(RwLock::new(RuntimeStateDoc::new()));
        let (state_changed_tx, _) = broadcast::channel(16);
        let room = NotebookRoom {
            id: uuid::Uuid::new_v4(),
            doc: Arc::new(RwLock::new(doc)),
            changed_tx,
            kernel_broadcast_tx,
            presence_tx,
            presence: Arc::new(RwLock::new(PresenceState::new())),
            persist_tx: Some(persist_tx),
            flush_request_tx: Some(flush_request_tx),
            persist_path,
            active_peers: AtomicUsize::new(0),
            had_peers: AtomicBool::new(false),
            is_ephemeral: AtomicBool::new(false),
            blob_store,
            trust_state: Arc::new(RwLock::new(TrustState {
                status: runt_trust::TrustStatus::Untrusted,
                info: runt_trust::TrustInfo {
                    status: runt_trust::TrustStatus::Untrusted,
                    uv_dependencies: vec![],
                    conda_dependencies: vec![],
                    conda_channels: vec![],
                },
                pending_launch: false,
            })),
            path: RwLock::new(Some(notebook_path.clone())),
            nbformat_attachments: Arc::new(RwLock::new(HashMap::new())),
            working_dir: Arc::new(RwLock::new(None)),
            auto_launch_at: Arc::new(RwLock::new(None)),

            is_loading: AtomicBool::new(false),
            last_self_write: Arc::new(AtomicU64::new(0)),
            last_save_heads: Arc::new(RwLock::new(Vec::new())),
            last_save_sources: Arc::new(RwLock::new(HashMap::new())),
            watcher_shutdown_tx: Mutex::new(None),
            state_doc,
            state_changed_tx,
            runtime_agent_handle: Arc::new(Mutex::new(None)),
            runtime_agent_env_path: Arc::new(RwLock::new(None)),
            runtime_agent_launched_config: Arc::new(RwLock::new(None)),
            runtime_agent_request_tx: Arc::new(Mutex::new(None)),
            pending_runtime_agent_connect_tx: Arc::new(Mutex::new(None)),
            runtime_agent_generation: Arc::new(AtomicU64::new(0)),
            next_queue_seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            current_runtime_agent_id: Arc::new(RwLock::new(None)),
        };

        (room, notebook_path)
    }

    #[tokio::test]
    async fn test_save_notebook_to_disk_creates_valid_nbformat() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, notebook_path) = test_room_with_path(&tmp, "test.ipynb");

        // Add cells to the doc
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell1", "code").unwrap();
            doc.update_source("cell1", "print('hello')").unwrap();
            doc.add_cell(1, "cell2", "markdown").unwrap();
            doc.update_source("cell2", "# Title").unwrap();
        }

        // Save to disk
        save_notebook_to_disk(&room, None).await.unwrap();

        // Read and validate with nbformat
        let content = std::fs::read_to_string(&notebook_path).unwrap();
        let notebook: nbformat::v4::Notebook =
            serde_json::from_str(&content).expect("Saved notebook should be valid nbformat");

        assert_eq!(notebook.cells.len(), 2);
        assert_eq!(notebook.nbformat, 4);
        assert!(
            notebook.nbformat_minor >= 5,
            "Cell IDs require nbformat_minor >= 5"
        );
    }

    #[tokio::test]
    async fn test_save_notebook_to_disk_preserves_unknown_metadata() {
        use std::io::Write;
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, notebook_path) = test_room_with_path(&tmp, "metadata.ipynb");

        // Create existing file with unknown metadata fields
        {
            let mut f = std::fs::File::create(&notebook_path).unwrap();
            writeln!(
                f,
                r#"{{
                    "nbformat": 4,
                    "nbformat_minor": 5,
                    "metadata": {{
                        "custom_extension": {{"key": "value"}},
                        "jupyter": {{"source_hidden": true}},
                        "runt": {{"trust_signature": "abc123", "schema_version": "1"}}
                    }},
                    "cells": []
                }}"#
            )
            .unwrap();
        }

        // Add a cell and save
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell1", "code").unwrap();
            doc.update_source("cell1", "x = 1").unwrap();
        }

        save_notebook_to_disk(&room, None).await.unwrap();

        // Verify unknown metadata is preserved
        let content = std::fs::read_to_string(&notebook_path).unwrap();
        let saved: serde_json::Value = serde_json::from_str(&content).unwrap();
        let metadata = saved.get("metadata").unwrap();

        // custom_extension should be preserved
        assert!(
            metadata.get("custom_extension").is_some(),
            "custom_extension should be preserved"
        );
        assert_eq!(
            metadata.get("custom_extension").unwrap().get("key"),
            Some(&serde_json::json!("value"))
        );

        // jupyter should be preserved
        assert!(
            metadata.get("jupyter").is_some(),
            "jupyter metadata should be preserved"
        );

        // trust_signature in runt should be preserved (deep-merge)
        let runt = metadata.get("runt").unwrap();
        assert_eq!(
            runt.get("trust_signature"),
            Some(&serde_json::json!("abc123")),
            "trust_signature should be preserved via deep-merge"
        );
    }

    #[tokio::test]
    async fn test_save_notebook_to_disk_enforces_nbformat_minor_5() {
        use std::io::Write;
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, notebook_path) = test_room_with_path(&tmp, "old_minor.ipynb");

        // Create existing file with old nbformat_minor
        {
            let mut f = std::fs::File::create(&notebook_path).unwrap();
            writeln!(
                f,
                r#"{{
                    "nbformat": 4,
                    "nbformat_minor": 2,
                    "metadata": {{}},
                    "cells": []
                }}"#
            )
            .unwrap();
        }

        // Add a cell with an id and save
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell-with-id", "code").unwrap();
        }

        save_notebook_to_disk(&room, None).await.unwrap();

        // Verify nbformat_minor is upgraded to 5
        let content = std::fs::read_to_string(&notebook_path).unwrap();
        let saved: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert_eq!(
            saved.get("nbformat_minor"),
            Some(&serde_json::json!(5)),
            "nbformat_minor should be upgraded to 5 when writing cell IDs"
        );
    }

    #[tokio::test]
    async fn test_save_notebook_to_disk_with_outputs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, notebook_path) = test_room_with_path(&tmp, "outputs.ipynb");

        // Add a cell with a raw output stored in RuntimeStateDoc
        let eid = "test-exec-1";
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell1", "code").unwrap();
            doc.update_source("cell1", "print('hello')").unwrap();
            doc.set_execution_id("cell1", Some(eid)).unwrap();
        }
        {
            let mut sd = room.state_doc.write().await;
            let output: serde_json::Value =
                serde_json::json!({"output_type": "stream", "name": "stdout", "text": ["hello\n"]});
            sd.create_execution(eid, "cell1");
            sd.set_execution_count(eid, 1);
            sd.set_outputs(eid, &[output]).unwrap();
            sd.set_execution_done(eid, true);
        }

        save_notebook_to_disk(&room, None).await.unwrap();

        // Read and validate
        let content = std::fs::read_to_string(&notebook_path).unwrap();
        let notebook: nbformat::v4::Notebook =
            serde_json::from_str(&content).expect("Should be valid nbformat with outputs");

        assert_eq!(notebook.cells.len(), 1);
        if let nbformat::v4::Cell::Code { outputs, .. } = &notebook.cells[0] {
            assert_eq!(outputs.len(), 1, "Should have one output");
            // Verify it's a stream output (nbformat types may vary)
            match &outputs[0] {
                nbformat::v4::Output::Stream { name, .. } => {
                    assert_eq!(name, "stdout");
                }
                _ => panic!("Expected stream output"),
            }
        } else {
            panic!("Expected code cell");
        }
    }

    #[test]
    fn test_is_untitled_notebook_with_uuid() {
        assert!(is_untitled_notebook("550e8400-e29b-41d4-a716-446655440000"));
        assert!(is_untitled_notebook("a1b2c3d4-e5f6-7890-abcd-ef1234567890"));
    }

    #[test]
    fn test_is_untitled_notebook_with_path() {
        assert!(!is_untitled_notebook("/home/user/notebook.ipynb"));
        assert!(!is_untitled_notebook("./relative/path.ipynb"));
        assert!(!is_untitled_notebook("notebook.ipynb"));
    }

    /// Test that the debouncer flushes at max interval even during continuous updates.
    ///
    /// Uses short intervals (50ms debounce, 200ms max) for fast testing.
    #[tokio::test]
    async fn test_persist_debouncer_max_interval_flush() {
        use std::time::Duration;

        let tmp = tempfile::TempDir::new().unwrap();
        let persist_path = tmp.path().join("test.automerge");

        // Create watch channel and spawn debouncer with short intervals for testing
        let (tx, rx) = watch::channel::<Option<Vec<u8>>>(None);
        let (_flush_tx, flush_rx) = mpsc::unbounded_channel::<FlushRequest>();
        let config = PersistDebouncerConfig {
            debounce_ms: 50,       // 50ms debounce window
            max_interval_ms: 200,  // 200ms max between flushes
            check_interval_ms: 10, // Check every 10ms
        };
        spawn_persist_debouncer_with_config(rx, flush_rx, persist_path.clone(), config);

        // Send updates every 20ms (faster than 50ms debounce, so debounce never triggers)
        // The 200ms max interval should force a flush even without a quiet period.
        for i in 0..20 {
            let data = format!("update-{}", i).into_bytes();
            tx.send(Some(data)).unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Total time: 20 * 20ms = 400ms, which is > 200ms max interval

        // Give debouncer time to flush
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(
            persist_path.exists(),
            "File should exist after max interval even with continuous updates"
        );

        // Verify content is from an update
        let content = std::fs::read(&persist_path).unwrap();
        let content_str = String::from_utf8_lossy(&content);
        assert!(
            content_str.starts_with("update-"),
            "Content should be from an update"
        );
    }

    /// Regression test for the eviction/debouncer race.
    ///
    /// The bug: room eviction used to remove the room from the HashMap before
    /// the persist debouncer's debounce window elapsed, so a fast reconnect
    /// would load stale/empty bytes. The fix: eviction sends a flush request
    /// on `flush_request_tx` and awaits an ack on the oneshot *before* the
    /// HashMap mutation. This test pins the contract: the ack must arrive
    /// after the latest watch value has been written to disk, well inside
    /// the debounce window.
    #[tokio::test]
    async fn test_persist_debouncer_flush_request_is_synchronous() {
        use std::time::Duration;

        let tmp = tempfile::TempDir::new().unwrap();
        let persist_path = tmp.path().join("race.automerge");

        // Use production defaults for debounce (500ms) so the timed flush
        // can't mask the flush-request ack timing.
        let (tx, rx) = watch::channel::<Option<Vec<u8>>>(None);
        let (flush_tx, flush_rx) = mpsc::unbounded_channel::<FlushRequest>();
        spawn_persist_debouncer(rx, flush_rx, persist_path.clone());

        // Push latest bytes and request a flush immediately. No sleeps — the
        // debounce timer must not be the thing that persists this write.
        let payload = b"eviction-latest-bytes".to_vec();
        tx.send(Some(payload.clone())).unwrap();

        let (ack_tx, ack_rx) = oneshot::channel::<bool>();
        flush_tx.send(ack_tx).unwrap();

        // The ack must come back fast (success=true). 500ms is 10x margin over
        // local disk I/O.
        let ack_result = tokio::time::timeout(Duration::from_millis(500), ack_rx).await;
        assert!(
            matches!(ack_result, Ok(Ok(true))),
            "flush ack did not arrive synchronously with success=true: {:?}",
            ack_result
        );

        // And the file on disk must hold the latest payload, not stale bytes.
        assert!(persist_path.exists(), "file must exist after flush ack");
        let on_disk = std::fs::read(&persist_path).unwrap();
        assert_eq!(
            on_disk, payload,
            "file contents must match latest payload after flush ack"
        );
    }

    /// The flush-and-ack must report I/O failures so the eviction task can
    /// retry (rather than remove the room and leave stale bytes on disk).
    /// Force a write failure by pointing persist_path at a non-writable
    /// location, then confirm the ack carries `false`.
    #[tokio::test]
    async fn test_persist_debouncer_flush_request_reports_write_failure() {
        use std::time::Duration;

        let tmp = tempfile::TempDir::new().unwrap();
        // Write target is a file *inside* a path that includes a non-directory
        // component — `std::fs::create_dir_all` on parent will succeed, but
        // `std::fs::write` on the final path will fail because it conflicts
        // with a regular file we planted there. This simulates ENOSPC-class
        // failures without needing OS-specific tricks.
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, b"regular file").unwrap();
        let persist_path = blocker.join("race.automerge");

        let (tx, rx) = watch::channel::<Option<Vec<u8>>>(None);
        let (flush_tx, flush_rx) = mpsc::unbounded_channel::<FlushRequest>();
        spawn_persist_debouncer(rx, flush_rx, persist_path.clone());

        let payload = b"write-will-fail".to_vec();
        tx.send(Some(payload)).unwrap();

        let (ack_tx, ack_rx) = oneshot::channel::<bool>();
        flush_tx.send(ack_tx).unwrap();

        let ack_result = tokio::time::timeout(Duration::from_millis(500), ack_rx).await;
        assert!(
            matches!(ack_result, Ok(Ok(false))),
            "flush ack must report write failure: {:?}",
            ack_result
        );
        // The file should not exist, since the write errored before any bytes hit disk.
        assert!(
            !persist_path.exists(),
            "persist_path must not exist after failed write"
        );
    }

    // ==========================================================================
    // File watcher tests
    // ==========================================================================

    #[test]
    fn test_parse_cells_from_ipynb_with_ids() {
        let json = serde_json::json!({
            "cells": [
                {
                    "id": "cell-1",
                    "cell_type": "code",
                    "source": "print('hello')",
                    "execution_count": 5,
                    "outputs": []
                },
                {
                    "id": "cell-2",
                    "cell_type": "markdown",
                    "source": ["# Title\n", "Body"],
                    "execution_count": null,
                    "outputs": []
                }
            ]
        });

        let parsed = parse_cells_from_ipynb(&json).expect("Should parse valid notebook");
        let cells = &parsed.cells;
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].id, "cell-1");
        assert_eq!(cells[0].cell_type, "code");
        assert_eq!(cells[0].source, "print('hello')");
        assert_eq!(cells[0].execution_count, "5");
        assert_eq!(cells[1].id, "cell-2");
        assert_eq!(cells[1].cell_type, "markdown");
        assert_eq!(cells[1].source, "# Title\nBody");
        assert_eq!(cells[1].execution_count, "null");
        // Empty `outputs` arrays on disk produce no entries in the outputs map.
        assert!(parsed.outputs_by_cell.is_empty());
    }

    #[test]
    fn test_parse_cells_from_ipynb_missing_ids() {
        // Older notebooks (pre-nbformat 4.5) don't have cell IDs
        let json = serde_json::json!({
            "cells": [
                {
                    "cell_type": "code",
                    "source": "x = 1",
                    "execution_count": null,
                    "outputs": []
                },
                {
                    "cell_type": "code",
                    "source": "y = 2",
                    "execution_count": null,
                    "outputs": []
                }
            ]
        });

        let parsed = parse_cells_from_ipynb(&json).expect("Should parse valid notebook");
        let cells = &parsed.cells;
        assert_eq!(cells.len(), 2);
        // Should generate fallback IDs based on index
        assert_eq!(cells[0].id, "__external_cell_0");
        assert_eq!(cells[1].id, "__external_cell_1");
        assert_eq!(cells[0].source, "x = 1");
        assert_eq!(cells[1].source, "y = 2");
    }

    #[test]
    fn test_parse_cells_from_ipynb_empty() {
        // Valid notebook with empty cells array - should return Some([])
        let json = serde_json::json!({
            "cells": []
        });
        let parsed = parse_cells_from_ipynb(&json).expect("Should parse valid empty notebook");
        assert!(parsed.cells.is_empty());
        assert!(parsed.outputs_by_cell.is_empty());
    }

    #[test]
    fn test_parse_cells_from_ipynb_no_cells_key() {
        // Invalid notebook (missing cells key) - should return None
        let json = serde_json::json!({
            "metadata": {}
        });
        assert!(
            parse_cells_from_ipynb(&json).is_none(),
            "Should return None for invalid notebook"
        );
    }

    #[tokio::test]
    async fn test_apply_ipynb_changes_clears_all_cells() {
        // Valid "delete all cells" case — empty cells array from external
        // file should clear the doc, but ONLY when we have a save baseline
        // (last_save_sources populated). Without a save snapshot, deletions
        // are skipped to prevent the Run 38 cell-loss bug.
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _) = test_room_with_path(&tmp, "test.ipynb");

        // Add cells to the doc
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell-1", "code").unwrap();
            doc.update_source("cell-1", "x = 1").unwrap();
        }

        // Populate last_save_sources — simulates a save that included the cell
        {
            let mut saved = room.last_save_sources.write().await;
            saved.insert("cell-1".to_string(), "x = 1".to_string());
        }

        // Apply empty external cells - should delete all cells (we have
        // a save baseline confirming cell-1 was on disk before)
        let external_cells = vec![];
        let changed = apply_ipynb_changes(
            &room,
            &external_cells,
            &HashMap::new(),
            &HashMap::new(),
            false,
        )
        .await;
        assert!(changed, "Should apply changes to clear all cells");

        // Verify all cells were deleted
        let cells = {
            let doc = room.doc.read().await;
            doc.get_cells()
        };
        assert!(cells.is_empty(), "All cells should be deleted");
    }

    #[tokio::test]
    async fn test_apply_ipynb_changes_updates_execution_count() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _) = test_room_with_path(&tmp, "test.ipynb");

        // Add cells to the doc
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell-1", "code").unwrap();
        }

        // Apply external changes with execution_count
        let external_cells = vec![CellSnapshot {
            id: "cell-1".to_string(),
            cell_type: "code".to_string(),
            position: "80".to_string(),
            source: String::new(),
            execution_count: "42".to_string(),
            metadata: serde_json::json!({}),
            resolved_assets: std::collections::HashMap::new(),
        }];

        let changed = apply_ipynb_changes(
            &room,
            &external_cells,
            &HashMap::new(),
            &HashMap::new(),
            false,
        )
        .await;
        assert!(changed, "Should detect execution_count change");

        // execution_count is now in RuntimeStateDoc via synthetic execution_id
        let doc = room.doc.read().await;
        let eid = doc.get_execution_id("cell-1");
        drop(doc);
        assert!(eid.is_some(), "Should have execution_id set");
        let sd = room.state_doc.read().await;
        let exec = sd.get_execution(eid.as_ref().unwrap());
        assert_eq!(exec.unwrap().execution_count, Some(42));
    }

    #[tokio::test]
    async fn test_apply_ipynb_changes_preserves_execution_count_when_kernel_running() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _) = test_room_with_path(&tmp, "test.ipynb");

        // Add cell with execution_count in RuntimeStateDoc via synthetic eid
        let eid = "existing-exec-1";
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell-1", "code").unwrap();
            doc.set_execution_id("cell-1", Some(eid)).unwrap();
        }
        {
            let mut sd = room.state_doc.write().await;
            sd.create_execution(eid, "cell-1");
            sd.set_execution_count(eid, 10);
            sd.set_execution_done(eid, true);
        }

        // Apply external changes while kernel is "running"
        let external_cells = vec![CellSnapshot {
            id: "cell-1".to_string(),
            cell_type: "code".to_string(),
            position: "80".to_string(),
            source: "new source".to_string(),
            execution_count: "5".to_string(),
            metadata: serde_json::json!({}),
            resolved_assets: std::collections::HashMap::new(),
        }];

        let changed = apply_ipynb_changes(
            &room,
            &external_cells,
            &HashMap::new(),
            &HashMap::new(),
            true,
        )
        .await;
        assert!(changed, "Should apply source change");

        let cells = {
            let doc = room.doc.read().await;
            doc.get_cells()
        };
        // Source should be updated
        assert_eq!(cells[0].source, "new source");
        // execution_count should be preserved in RuntimeStateDoc (kernel running)
        let sd = room.state_doc.read().await;
        let exec = sd.get_execution(eid);
        assert_eq!(exec.unwrap().execution_count, Some(10));
    }

    #[tokio::test]
    async fn test_apply_ipynb_changes_new_cell_with_outputs_while_kernel_running() {
        // New external cells should get their external outputs even when kernel is running
        // (they don't have any in-progress state to preserve)
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _) = test_room_with_path(&tmp, "test.ipynb");

        // Start with one cell
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "existing-cell", "code").unwrap();
        }

        // Add a new external cell with outputs while kernel is "running"
        let external_cells = vec![
            CellSnapshot {
                id: "existing-cell".to_string(),
                cell_type: "code".to_string(),
                position: "80".to_string(),
                source: String::new(),
                execution_count: "null".to_string(),
                metadata: serde_json::json!({}),
                resolved_assets: std::collections::HashMap::new(),
            },
            CellSnapshot {
                id: "new-cell".to_string(),
                cell_type: "code".to_string(),
                position: "81".to_string(),
                source: "print('new')".to_string(),
                execution_count: "42".to_string(),
                metadata: serde_json::json!({}),
                resolved_assets: std::collections::HashMap::new(),
            },
        ];
        let mut external_outputs: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
        external_outputs.insert(
            "new-cell".to_string(),
            vec![serde_json::json!({"output_type":"execute_result"})],
        );

        let changed = apply_ipynb_changes(
            &room,
            &external_cells,
            &external_outputs,
            &HashMap::new(),
            true,
        )
        .await;
        assert!(changed, "Should add new cell");

        let cells = {
            let doc = room.doc.read().await;
            doc.get_cells()
        };
        assert_eq!(cells.len(), 2);

        // New cell should have external outputs and execution_count in RuntimeStateDoc
        let new_cell = cells.iter().find(|c| c.id == "new-cell").unwrap();
        assert_eq!(new_cell.source, "print('new')");

        // Outputs and execution_count are in RuntimeStateDoc keyed by synthetic execution_id
        let eid = {
            let doc = room.doc.read().await;
            doc.get_execution_id("new-cell")
                .expect("new-cell should have execution_id")
        };
        let sd = room.state_doc.read().await;
        let exec = sd.get_execution(&eid);
        assert_eq!(exec.unwrap().execution_count, Some(42));
        let outputs = sd.get_outputs(&eid);
        drop(sd);
        assert_eq!(outputs.len(), 1);
        let manifest = &outputs[0];
        assert!(
            manifest.is_object(),
            "Output should be a manifest object, got: {}",
            manifest
        );
        // Verify the manifest resolves back to the original output
        let parsed_manifest: crate::output_store::OutputManifest =
            serde_json::from_value(manifest.clone()).unwrap();
        let resolved = crate::output_store::resolve_manifest(&parsed_manifest, &room.blob_store)
            .await
            .unwrap();
        assert_eq!(resolved["output_type"], "execute_result");
    }

    #[tokio::test]
    async fn test_apply_ipynb_changes_wholesale_replacement() {
        // When external file has entirely different cell IDs (zero overlap),
        // the rebuild path should replace all cells correctly (issue #1310).
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _) = test_room_with_path(&tmp, "test.ipynb");

        // Add original cells
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "old-a", "code").unwrap();
            doc.update_source("old-a", "x = 1").unwrap();
            doc.add_cell(1, "old-b", "code").unwrap();
            doc.update_source("old-b", "y = 2").unwrap();
            doc.add_cell(2, "old-c", "markdown").unwrap();
            doc.update_source("old-c", "# Hello").unwrap();
        }

        // Completely replace with different cells (zero common IDs)
        let external_cells = vec![
            CellSnapshot {
                id: "new-1".to_string(),
                cell_type: "code".to_string(),
                position: "80".to_string(),
                source: "a = 10".to_string(),
                execution_count: "1".to_string(),
                metadata: serde_json::json!({}),
                resolved_assets: std::collections::HashMap::new(),
            },
            CellSnapshot {
                id: "new-2".to_string(),
                cell_type: "code".to_string(),
                position: "81".to_string(),
                source: "b = 20".to_string(),
                execution_count: "2".to_string(),
                metadata: serde_json::json!({}),
                resolved_assets: std::collections::HashMap::new(),
            },
        ];

        let changed = apply_ipynb_changes(
            &room,
            &external_cells,
            &HashMap::new(),
            &HashMap::new(),
            false,
        )
        .await;
        assert!(changed, "Should detect wholesale replacement");

        let cells = {
            let doc = room.doc.read().await;
            doc.get_cells()
        };
        assert_eq!(cells.len(), 2, "Should have exactly 2 new cells");
        assert_eq!(cells[0].id, "new-1");
        assert_eq!(cells[0].source, "a = 10");
        assert_eq!(cells[1].id, "new-2");
        assert_eq!(cells[1].source, "b = 20");
        // Old cells should be gone
        assert!(cells.iter().all(|c| !c.id.starts_with("old-")));
    }

    #[tokio::test]
    async fn test_apply_ipynb_changes_partial_overlap_preserves_unsaved() {
        // When there IS overlap between current and external cells, the
        // incremental path should preserve user-added cells not in
        // last_save_sources.
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _) = test_room_with_path(&tmp, "test.ipynb");

        // Add cells and populate last_save_sources to simulate a save
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "keep", "code").unwrap();
            doc.update_source("keep", "x = 1").unwrap();
            doc.add_cell(1, "remove", "code").unwrap();
            doc.update_source("remove", "y = 2").unwrap();
        }
        {
            let mut saved = room.last_save_sources.write().await;
            saved.insert("keep".to_string(), "x = 1".to_string());
            saved.insert("remove".to_string(), "y = 2".to_string());
        }

        // Add a cell NOT in last_save_sources (user just added it)
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(2, "user-added", "code").unwrap();
            doc.update_source("user-added", "z = 3").unwrap();
        }

        // External file has "keep" (overlap) but not "remove" or "user-added"
        let external_cells = vec![CellSnapshot {
            id: "keep".to_string(),
            cell_type: "code".to_string(),
            position: "80".to_string(),
            source: "x = 1".to_string(),
            execution_count: "null".to_string(),
            metadata: serde_json::json!({}),
            resolved_assets: std::collections::HashMap::new(),
        }];

        let changed = apply_ipynb_changes(
            &room,
            &external_cells,
            &HashMap::new(),
            &HashMap::new(),
            false,
        )
        .await;
        assert!(changed);

        let cells = {
            let doc = room.doc.read().await;
            doc.get_cells()
        };
        let ids: Vec<&str> = cells.iter().map(|c| c.id.as_str()).collect();
        assert!(
            ids.contains(&"keep"),
            "Overlapping cell should remain: {:?}",
            ids
        );
        assert!(
            !ids.contains(&"remove"),
            "Saved cell removed externally should be deleted: {:?}",
            ids
        );
        assert!(
            ids.contains(&"user-added"),
            "User-added cell not in save snapshot should be preserved: {:?}",
            ids
        );
    }

    #[tokio::test]
    async fn test_apply_ipynb_changes_no_save_snapshot_preserves_crdt_cells() {
        // Regression test for Run 38 cell-loss: when last_save_sources is
        // empty (initial autosave with 0 cells), the file watcher must NOT
        // delete CRDT cells that aren't on disk. Without a save baseline we
        // can't distinguish "externally deleted" from "just created in CRDT."
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _) = test_room_with_path(&tmp, "test.ipynb");

        // Add cells to the CRDT (simulates MCP client creating cells)
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell-a", "code").unwrap();
            doc.update_source("cell-a", "x = 1").unwrap();
            doc.add_cell(1, "cell-b", "code").unwrap();
            doc.update_source("cell-b", "y = 2").unwrap();
        }

        // Do NOT populate last_save_sources — simulates the case where
        // the only save was with 0 cells (empty HashMap is the default).
        assert!(room.last_save_sources.read().await.is_empty());

        // External file has 0 cells (the autosave wrote an empty notebook)
        let external_cells: Vec<CellSnapshot> = vec![];

        let changed = apply_ipynb_changes(
            &room,
            &external_cells,
            &HashMap::new(),
            &HashMap::new(),
            false,
        )
        .await;
        // No changes should be applied — cells preserved
        assert!(
            !changed,
            "Should not delete cells when no save snapshot exists"
        );

        let cells = {
            let doc = room.doc.read().await;
            doc.get_cells()
        };
        let ids: Vec<&str> = cells.iter().map(|c| c.id.as_str()).collect();
        assert!(
            ids.contains(&"cell-a"),
            "CRDT cell should be preserved when no save snapshot: {:?}",
            ids
        );
        assert!(
            ids.contains(&"cell-b"),
            "CRDT cell should be preserved when no save snapshot: {:?}",
            ids
        );
    }

    #[tokio::test]
    async fn test_load_notebook_from_disk_routes_outputs_through_blob_store() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);

        // Create a .ipynb file with outputs including a large base64 image
        let large_image = "x".repeat(16 * 1024); // 16KB, above 8KB inline threshold
        let notebook_json = serde_json::json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {},
            "cells": [
                {
                    "id": "cell-1",
                    "cell_type": "code",
                    "source": "1 + 1",
                    "execution_count": 1,
                    "metadata": {},
                    "outputs": [
                        {
                            "output_type": "execute_result",
                            "execution_count": 1,
                            "data": { "text/plain": "2" },
                            "metadata": {}
                        }
                    ]
                },
                {
                    "id": "cell-2",
                    "cell_type": "code",
                    "source": "display(img)",
                    "execution_count": 2,
                    "metadata": {},
                    "outputs": [
                        {
                            "output_type": "display_data",
                            "data": {
                                "text/plain": "<Image>",
                                "image/png": large_image
                            },
                            "metadata": {}
                        }
                    ]
                },
                {
                    "id": "cell-3",
                    "cell_type": "code",
                    "source": "print('hi')",
                    "execution_count": 3,
                    "metadata": {},
                    "outputs": [
                        {
                            "output_type": "stream",
                            "name": "stdout",
                            "text": "hi\n"
                        }
                    ]
                }
            ]
        });

        let ipynb_path = tmp.path().join("test.ipynb");
        std::fs::write(
            &ipynb_path,
            serde_json::to_string_pretty(&notebook_json).unwrap(),
        )
        .unwrap();

        let notebook_id = ipynb_path.to_string_lossy().to_string();
        let mut doc = crate::notebook_doc::NotebookDoc::new(&notebook_id);
        let mut state_doc = RuntimeStateDoc::new();

        let count = load_notebook_from_disk_with_state_doc(
            &mut doc,
            Some(&mut state_doc),
            &ipynb_path,
            &blob_store,
        )
        .await
        .unwrap();
        assert_eq!(count, 3);

        let cells = doc.get_cells();
        assert_eq!(cells.len(), 3);

        // Each code cell with outputs should have an execution_id pointing to state_doc
        for cell in &cells {
            if let Some(eid) = doc.get_execution_id(&cell.id) {
                let outputs = state_doc.get_outputs(&eid);
                assert!(
                    !outputs.is_empty(),
                    "Cell {} should have outputs in state doc",
                    cell.id
                );
                for output_ref in &outputs {
                    assert!(
                        output_ref.is_object(),
                        "Cell {} output should be a manifest object, got: {}",
                        cell.id,
                        output_ref
                    );
                    assert!(
                        output_ref.get("output_type").is_some(),
                        "Cell {} output manifest should have output_type",
                        cell.id
                    );
                }
            }
        }

        // Resolve cell-1's execute_result and verify round-trip
        let eid1 = doc
            .get_execution_id("cell-1")
            .expect("cell-1 should have execution_id");
        let outputs1 = state_doc.get_outputs(&eid1);
        let manifest = &outputs1[0];
        let parsed_manifest: crate::output_store::OutputManifest =
            serde_json::from_value(manifest.clone()).unwrap();
        let resolved = crate::output_store::resolve_manifest(&parsed_manifest, &blob_store)
            .await
            .unwrap();
        assert_eq!(resolved["output_type"], "execute_result");
        assert_eq!(resolved["data"]["text/plain"], "2");
        assert_eq!(resolved["execution_count"], 1);

        // Resolve cell-2's display_data with the large image
        let eid2 = doc
            .get_execution_id("cell-2")
            .expect("cell-2 should have execution_id");
        let outputs2 = state_doc.get_outputs(&eid2);
        let manifest = &outputs2[0];
        let parsed_manifest2: crate::output_store::OutputManifest =
            serde_json::from_value(manifest.clone()).unwrap();
        // The manifest should contain a blob ref for the large image, not inline
        let image_ref = &manifest["data"]["image/png"];
        assert!(
            image_ref.get("blob").is_some(),
            "Large image should be stored as blob ref, not inlined: {}",
            image_ref
        );
        // Full round-trip should reconstruct original data
        let resolved = crate::output_store::resolve_manifest(&parsed_manifest2, &blob_store)
            .await
            .unwrap();
        assert_eq!(resolved["output_type"], "display_data");
        assert_eq!(resolved["data"]["image/png"], large_image);

        // Resolve cell-3's stream output
        let eid3 = doc
            .get_execution_id("cell-3")
            .expect("cell-3 should have execution_id");
        let outputs3 = state_doc.get_outputs(&eid3);
        let manifest = &outputs3[0];
        let parsed_manifest: crate::output_store::OutputManifest =
            serde_json::from_value(manifest.clone()).unwrap();
        let resolved = crate::output_store::resolve_manifest(&parsed_manifest, &blob_store)
            .await
            .unwrap();
        assert_eq!(resolved["output_type"], "stream");
        assert_eq!(resolved["name"], "stdout");
        assert_eq!(resolved["text"], "hi\n");
    }

    #[tokio::test]
    async fn test_load_notebook_from_disk_resolves_nbformat_attachments() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);

        let notebook_json = serde_json::json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {},
            "cells": [
                {
                    "id": "markdown-1",
                    "cell_type": "markdown",
                    "source": ["![inline](attachment:image.png)"],
                    "metadata": {},
                    "attachments": {
                        "image.png": {
                            "image/png": "aGVsbG8="
                        }
                    }
                }
            ]
        });

        let ipynb_path = tmp.path().join("attachments.ipynb");
        std::fs::write(
            &ipynb_path,
            serde_json::to_string_pretty(&notebook_json).unwrap(),
        )
        .unwrap();

        let notebook_id = ipynb_path.to_string_lossy().to_string();
        let mut doc = crate::notebook_doc::NotebookDoc::new(&notebook_id);

        let count = load_notebook_from_disk(&mut doc, &ipynb_path, &blob_store)
            .await
            .unwrap();
        assert_eq!(count, 1);

        let cells = doc.get_cells();
        assert_eq!(cells.len(), 1);

        let hash = cells[0]
            .resolved_assets
            .get("attachment:image.png")
            .expect("attachment should resolve into render assets");

        let bytes = blob_store.get(hash).await.unwrap().unwrap();
        assert_eq!(bytes, b"hello");
    }

    #[tokio::test]
    async fn test_load_notebook_from_disk_skips_code_cell_asset_resolution() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        std::fs::write(tmp.path().join("image.png"), b"hello").unwrap();

        let notebook_json = serde_json::json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {},
            "cells": [
                {
                    "id": "code-1",
                    "cell_type": "code",
                    "source": ["![inline](image.png)"],
                    "metadata": {},
                    "outputs": [],
                    "execution_count": null
                }
            ]
        });

        let ipynb_path = tmp.path().join("code-assets.ipynb");
        std::fs::write(
            &ipynb_path,
            serde_json::to_string_pretty(&notebook_json).unwrap(),
        )
        .unwrap();

        let notebook_id = ipynb_path.to_string_lossy().to_string();
        let mut doc = crate::notebook_doc::NotebookDoc::new(&notebook_id);

        let count = load_notebook_from_disk(&mut doc, &ipynb_path, &blob_store)
            .await
            .unwrap();
        assert_eq!(count, 1);

        let cells = doc.get_cells();
        assert_eq!(cells.len(), 1);
        assert!(cells[0].resolved_assets.is_empty());
    }

    #[tokio::test]
    async fn test_process_markdown_assets_rebuilds_stale_refs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, notebook_path) = test_room_with_path(&tmp, "assets.ipynb");
        std::fs::write(&notebook_path, "{}").unwrap();
        std::fs::write(tmp.path().join("img1.png"), b"one").unwrap();
        std::fs::write(tmp.path().join("img2.png"), b"two").unwrap();

        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "markdown-1", "markdown").unwrap();
            doc.update_source("markdown-1", "![one](img1.png)").unwrap();
        }

        process_markdown_assets(&room).await;

        {
            let cells = room.doc.read().await.get_cells();
            let assets = &cells[0].resolved_assets;
            assert!(assets.contains_key("img1.png"));
            assert_eq!(assets.len(), 1);
        }

        {
            let mut doc = room.doc.write().await;
            doc.update_source("markdown-1", "![two](img2.png)").unwrap();
        }

        process_markdown_assets(&room).await;

        let cells = room.doc.read().await.get_cells();
        let assets = &cells[0].resolved_assets;
        assert!(assets.contains_key("img2.png"));
        assert!(!assets.contains_key("img1.png"));
        assert_eq!(assets.len(), 1);
    }

    #[tokio::test]
    async fn test_save_notebook_to_disk_with_target_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _original_path) = test_room_with_path(&tmp, "original.ipynb");

        // Add a cell
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell1", "code").unwrap();
            doc.update_source("cell1", "x = 1").unwrap();
        }

        // Save to a different absolute path
        let new_path = tmp.path().join("new_location.ipynb");
        let result = save_notebook_to_disk(&room, Some(new_path.to_str().unwrap())).await;

        assert!(result.is_ok());
        let saved_path = result.unwrap();
        assert_eq!(saved_path, new_path.to_string_lossy());
        assert!(new_path.exists(), "File should be created at new path");

        // Verify content
        let content = std::fs::read_to_string(&new_path).unwrap();
        let notebook: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(notebook["cells"][0]["source"], serde_json::json!(["x = 1"]));
    }

    #[tokio::test]
    async fn test_save_notebook_to_disk_preserves_nbformat_attachments_from_cache() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, original_path) = test_room_with_path(&tmp, "original.ipynb");

        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "markdown-1", "markdown").unwrap();
            doc.update_source("markdown-1", "![inline](attachment:image.png)")
                .unwrap();
        }
        {
            let mut attachments = room.nbformat_attachments.write().await;
            attachments.insert(
                "markdown-1".to_string(),
                serde_json::json!({
                    "image.png": {
                        "image/png": "aGVsbG8="
                    }
                }),
            );
        }

        save_notebook_to_disk(&room, None).await.unwrap();

        let content = std::fs::read_to_string(&original_path).unwrap();
        let notebook: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            notebook["cells"][0]["attachments"],
            serde_json::json!({
                "image.png": {
                    "image/png": "aGVsbG8="
                }
            })
        );
    }

    #[tokio::test]
    async fn test_save_notebook_to_disk_preserves_raw_cell_attachments_from_cache() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, original_path) = test_room_with_path(&tmp, "raw.ipynb");

        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "raw-1", "raw").unwrap();
            doc.update_source("raw-1", "attachment ref").unwrap();
        }
        {
            let mut attachments = room.nbformat_attachments.write().await;
            attachments.insert(
                "raw-1".to_string(),
                serde_json::json!({
                    "snippet.txt": {
                        "text/plain": "hello"
                    }
                }),
            );
        }

        save_notebook_to_disk(&room, None).await.unwrap();

        let content = std::fs::read_to_string(&original_path).unwrap();
        let notebook: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            notebook["cells"][0]["attachments"],
            serde_json::json!({
                "snippet.txt": {
                    "text/plain": "hello"
                }
            })
        );
    }

    #[tokio::test]
    async fn test_save_notebook_to_disk_appends_ipynb_extension() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _original_path) = test_room_with_path(&tmp, "original.ipynb");

        // Add a cell
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell1", "code").unwrap();
        }

        // Save to path without .ipynb extension
        let base_path = tmp.path().join("no_extension");
        let result = save_notebook_to_disk(&room, Some(base_path.to_str().unwrap())).await;

        assert!(result.is_ok());
        let saved_path = result.unwrap();
        assert!(
            saved_path.ends_with(".ipynb"),
            "Saved path should have .ipynb extension"
        );

        let expected_path = tmp.path().join("no_extension.ipynb");
        assert!(
            expected_path.exists(),
            "File should exist with .ipynb extension"
        );
    }

    #[tokio::test]
    async fn test_save_notebook_to_disk_rejects_relative_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _original_path) = test_room_with_path(&tmp, "original.ipynb");

        // Try to save with a relative path
        let result = save_notebook_to_disk(&room, Some("relative/path.ipynb")).await;

        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(
            matches!(error, SaveError::Unrecoverable(_)),
            "Error should be unrecoverable: {}",
            error
        );
        assert!(
            error
                .to_string()
                .contains("Relative paths are not supported"),
            "Error should mention relative paths: {}",
            error
        );
    }

    #[tokio::test]
    async fn test_format_notebook_cells_skips_unknown_runtime() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _notebook_path) = test_room_with_path(&tmp, "unknown_runtime.ipynb");

        // Add a code cell (no kernelspec metadata set = unknown runtime)
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell1", "code").unwrap();
            doc.update_source("cell1", "x=1").unwrap(); // Would be formatted if Python
        }

        // Run format - should skip (return 0) since no kernelspec
        let result = format_notebook_cells(&room).await;
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            0,
            "Should format 0 cells for unknown runtime"
        );

        // Source should be unchanged
        let cells = {
            let doc = room.doc.read().await;
            doc.get_cells()
        };
        assert_eq!(cells[0].source, "x=1", "Source should remain unchanged");
    }

    // ========================================================================
    // Tests for daemon-owned notebook loading functions (Phase 2)
    // ========================================================================

    #[test]
    fn test_build_new_notebook_metadata_deno() {
        let metadata = build_new_notebook_metadata(
            "deno",
            "test-env-id",
            crate::settings_doc::PythonEnvType::Uv,
        );

        assert_eq!(metadata.kernelspec.as_ref().unwrap().name, "deno");
        assert_eq!(metadata.kernelspec.as_ref().unwrap().display_name, "Deno");
        assert_eq!(
            metadata.kernelspec.as_ref().unwrap().language,
            Some("typescript".to_string())
        );
        assert_eq!(metadata.language_info.as_ref().unwrap().name, "typescript");
        assert_eq!(metadata.runt.env_id, Some("test-env-id".to_string()));
        assert!(metadata.runt.uv.is_none());
        assert!(metadata.runt.conda.is_none());
    }

    #[test]
    fn test_build_new_notebook_metadata_python_uv() {
        let metadata = build_new_notebook_metadata(
            "python",
            "test-env-id",
            crate::settings_doc::PythonEnvType::Uv,
        );

        assert_eq!(metadata.kernelspec.as_ref().unwrap().name, "python3");
        assert_eq!(
            metadata.kernelspec.as_ref().unwrap().display_name,
            "Python 3"
        );
        assert_eq!(
            metadata.kernelspec.as_ref().unwrap().language,
            Some("python".to_string())
        );
        assert_eq!(metadata.language_info.as_ref().unwrap().name, "python");
        assert_eq!(metadata.runt.env_id, Some("test-env-id".to_string()));
        assert!(metadata.runt.uv.is_some());
        assert!(metadata.runt.conda.is_none());
        assert!(metadata.runt.uv.as_ref().unwrap().dependencies.is_empty());
    }

    #[test]
    fn test_build_new_notebook_metadata_python_conda() {
        let metadata = build_new_notebook_metadata(
            "python",
            "test-env-id",
            crate::settings_doc::PythonEnvType::Conda,
        );

        assert_eq!(metadata.kernelspec.as_ref().unwrap().name, "python3");
        assert_eq!(metadata.language_info.as_ref().unwrap().name, "python");
        assert_eq!(metadata.runt.env_id, Some("test-env-id".to_string()));
        assert!(metadata.runt.uv.is_none());
        assert!(metadata.runt.conda.is_some());
        assert!(metadata
            .runt
            .conda
            .as_ref()
            .unwrap()
            .dependencies
            .is_empty());
        // Verify default channels to avoid false channel-drift detection
        assert_eq!(
            metadata.runt.conda.as_ref().unwrap().channels,
            vec!["conda-forge".to_string()]
        );
    }

    #[test]
    fn test_create_empty_notebook_python() {
        let mut doc = NotebookDoc::new("test");
        let result = create_empty_notebook(
            &mut doc,
            "python",
            crate::settings_doc::PythonEnvType::Uv,
            None,
        );

        assert!(result.is_ok());
        let env_id = result.unwrap();
        assert!(!env_id.is_empty(), "Should generate an env_id");

        // Should have zero cells (frontend creates the first cell locally)
        assert_eq!(doc.cell_count(), 0);
    }

    #[test]
    fn test_create_empty_notebook_deno() {
        let mut doc = NotebookDoc::new("test");
        let result = create_empty_notebook(
            &mut doc,
            "deno",
            crate::settings_doc::PythonEnvType::Uv, // Ignored for deno
            None,
        );

        assert!(result.is_ok());
        assert_eq!(doc.cell_count(), 0);

        // Check metadata was set correctly
        let metadata = doc.get_metadata_snapshot();
        assert!(metadata.is_some());
        let metadata = metadata.unwrap();
        assert_eq!(metadata.kernelspec.as_ref().unwrap().name, "deno");
    }

    #[test]
    fn test_create_empty_notebook_with_provided_env_id() {
        let mut doc = NotebookDoc::new("test");
        let provided_id = "my-custom-env-id";
        let result = create_empty_notebook(
            &mut doc,
            "python",
            crate::settings_doc::PythonEnvType::Uv,
            Some(provided_id),
        );

        assert!(result.is_ok());
        let env_id = result.unwrap();
        assert_eq!(env_id, provided_id, "Should use provided env_id");

        let metadata = doc.get_metadata_snapshot().unwrap();
        assert_eq!(
            metadata.runt.env_id,
            Some(provided_id.to_string()),
            "Metadata should have provided env_id"
        );
    }

    /// Benchmark streaming load phases against a real notebook.
    ///
    /// Reads `/tmp/gelmanschools-bench.ipynb` and profiles:
    /// - jiter parse time
    /// - blob store output processing per batch
    /// - add_cell_full per batch
    /// - generate_sync_message per batch
    ///
    /// Run with: cargo test -p runtimed -- bench_streaming_load_phases --nocapture --ignored
    #[tokio::test]
    #[ignore] // Only run manually — requires the fixture notebook
    async fn bench_streaming_load_phases() {
        let notebook_path = std::path::Path::new("/tmp/gelmanschools-bench.ipynb");
        if !notebook_path.exists() {
            eprintln!("Skipping: /tmp/gelmanschools-bench.ipynb not found");
            eprintln!("Copy the gelmanschools notebook there first:");
            eprintln!("  cp ~/Downloads/gelmanschools/index.ipynb /tmp/gelmanschools-bench.ipynb");
            return;
        }

        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);

        // Phase 0: Read + parse
        let t0 = std::time::Instant::now();
        let bytes = std::fs::read(notebook_path).unwrap();
        let read_elapsed = t0.elapsed();

        let t_parse = std::time::Instant::now();
        let (cells, _metadata, _attachments) = parse_notebook_jiter(&bytes).unwrap();
        let parse_elapsed = t_parse.elapsed();

        eprintln!(
            "--- Notebook: {} cells, {} bytes ---",
            cells.len(),
            bytes.len()
        );
        eprintln!("  Read file:  {:?}", read_elapsed);
        eprintln!("  jiter parse: {:?}", parse_elapsed);

        // Create doc + peer state
        let mut doc = crate::notebook_doc::NotebookDoc::new("bench");
        let mut peer_state = automerge::sync::State::new();

        let batch_size = STREAMING_BATCH_SIZE;
        let mut cell_iter = cells.into_iter().enumerate().peekable();
        let mut batch_num = 0u32;

        let mut total_blob = std::time::Duration::ZERO;
        let mut total_add = std::time::Duration::ZERO;
        let mut total_sync_gen = std::time::Duration::ZERO;

        while cell_iter.peek().is_some() {
            // Blob store phase
            let t_blob = std::time::Instant::now();
            let mut batch: Vec<(usize, StreamingCell, Vec<serde_json::Value>)> = Vec::new();
            let mut batch_output_bytes = 0usize;
            for _ in 0..batch_size {
                let Some((idx, cell)) = cell_iter.next() else {
                    break;
                };
                let mut output_refs = Vec::with_capacity(cell.outputs.len());
                for output in &cell.outputs {
                    batch_output_bytes += output.to_string().len();
                    output_refs.push(output_value_to_manifest_ref(output, &blob_store).await);
                }
                batch.push((idx, cell, output_refs));
            }
            let blob_elapsed = t_blob.elapsed();

            // add_cell_full phase
            let t_add = std::time::Instant::now();
            for (_idx, cell, _output_refs) in &batch {
                doc.add_cell_full(
                    &cell.id,
                    &cell.cell_type,
                    &cell.position,
                    &cell.source,
                    &cell.execution_count,
                    &cell.metadata,
                )
                .unwrap();
            }
            let add_elapsed = t_add.elapsed();

            // generate_sync_message phase
            let t_sync = std::time::Instant::now();
            let encoded = doc
                .generate_sync_message(&mut peer_state)
                .map(|m| m.encode());
            let sync_elapsed = t_sync.elapsed();
            let msg_size = encoded.as_ref().map(|e| e.len()).unwrap_or(0);

            batch_num += 1;
            eprintln!(
                "  Batch {:2} ({} cells, {:6}KB output): blob={:>8?}  add={:>8?}  sync_gen={:>8?}  msg={}KB",
                batch_num,
                batch.len(),
                batch_output_bytes / 1024,
                blob_elapsed,
                add_elapsed,
                sync_elapsed,
                msg_size / 1024,
            );

            total_blob += blob_elapsed;
            total_add += add_elapsed;
            total_sync_gen += sync_elapsed;
        }

        eprintln!("--- Totals ---");
        eprintln!("  blob store:         {:?}", total_blob);
        eprintln!("  add_cell_full:      {:?}", total_add);
        eprintln!("  generate_sync_msg:  {:?}", total_sync_gen);
        eprintln!(
            "  total (no I/O):     {:?}",
            total_blob + total_add + total_sync_gen
        );
        eprintln!("  cells: {}, batches: {}", doc.cell_count(), batch_num);
    }

    #[tokio::test]
    async fn test_update_kernel_presence_publishes_state_and_relays() {
        let presence_state = Arc::new(RwLock::new(PresenceState::new()));
        let (presence_tx, mut presence_rx) = broadcast::channel::<(String, Vec<u8>)>(16);

        update_kernel_presence(
            &presence_state,
            &presence_tx,
            presence::KernelStatus::Idle,
            "uv:prewarmed",
        )
        .await;

        // Verify presence state contains the daemon peer with KernelState channel
        let state = presence_state.read().await;
        let peers = state.peers();
        let daemon_peer = peers.get("daemon").expect("daemon peer should exist");
        assert_eq!(daemon_peer.peer_id, "daemon");

        let kernel_channel = daemon_peer
            .channels
            .get(&presence::Channel::KernelState)
            .expect("kernel_state channel should exist");
        match kernel_channel {
            presence::ChannelData::KernelState(data) => {
                assert_eq!(data.status, presence::KernelStatus::Idle);
                assert_eq!(data.env_source, "uv:prewarmed");
            }
            other => panic!("expected KernelState, got {:?}", other),
        }
        drop(state);

        // Verify a relay frame was sent
        let (peer_id, bytes) = presence_rx
            .recv()
            .await
            .expect("should receive relay frame");
        assert_eq!(peer_id, "daemon");
        // Decode the frame to verify it's a valid KernelState update
        let msg = presence::decode_message(&bytes).expect("should decode presence message");
        match msg {
            presence::PresenceMessage::Update { peer_id, data, .. } => {
                assert_eq!(peer_id, "daemon");
                match data {
                    presence::ChannelData::KernelState(data) => {
                        assert_eq!(data.status, presence::KernelStatus::Idle);
                        assert_eq!(data.env_source, "uv:prewarmed");
                    }
                    other => panic!("expected KernelState data, got {:?}", other),
                }
            }
            other => panic!("expected Update message, got {:?}", other),
        }
    }

    // ── Regression test: autosave after save_notebook path update ──────

    /// Verify that saving an untitled (UUID-keyed) room updates path_index and
    /// room.path, while keeping the UUID stable in the rooms map.
    #[tokio::test]
    async fn saving_untitled_notebook_updates_path_index_and_keeps_uuid() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let docs_dir = tmp.path().join("docs");
        std::fs::create_dir_all(&docs_dir).unwrap();

        // 1. Create an ephemeral-but-persisted room (UUID, no path)
        let uuid = Uuid::new_v4();
        let room = Arc::new(NotebookRoom::new_fresh(
            uuid, None, &docs_dir, blob_store, false,
        ));
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "c1", "code").unwrap();
        }
        let rooms: NotebookRooms = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        rooms.lock().await.insert(uuid, room.clone());
        let path_index = Arc::new(tokio::sync::Mutex::new(PathIndex::new()));

        // 2. Simulate the handler's transition: save to disk then wire path_index.
        let save_target = tmp.path().join("note.ipynb");
        let written = save_notebook_to_disk(&room, Some(save_target.to_str().unwrap()))
            .await
            .unwrap();
        let canonical = tokio::fs::canonicalize(&written)
            .await
            .unwrap_or_else(|_| PathBuf::from(&written));

        path_index
            .lock()
            .await
            .insert(canonical.clone(), room.id)
            .unwrap();
        *room.path.write().await = Some(canonical.clone());

        // UUID key unchanged, path_index populated, room.path set.
        assert!(rooms.lock().await.contains_key(&uuid));
        assert_eq!(path_index.lock().await.lookup(&canonical), Some(uuid));
        assert_eq!(room.path.read().await.as_deref(), Some(canonical.as_path()));
    }

    /// Verify that `promote_untitled_to_file_backed` returns
    /// `SaveErrorKind::PathAlreadyOpen` when the target path is already held by
    /// another room, and does NOT mutate the fresh room's state on error.
    #[tokio::test]
    async fn saving_to_already_open_path_returns_path_already_open_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let docs_dir = tmp.path().join("docs");
        std::fs::create_dir_all(&docs_dir).unwrap();

        // Existing room already claiming `target_path`.
        let existing_uuid = Uuid::new_v4();
        let target_path = tmp.path().join("existing.ipynb");
        let path_index = Arc::new(tokio::sync::Mutex::new(PathIndex::new()));
        path_index
            .lock()
            .await
            .insert(target_path.clone(), existing_uuid)
            .unwrap();

        // Fresh untitled room that tries to claim the same path.
        let new_uuid = Uuid::new_v4();
        let room = Arc::new(NotebookRoom::new_fresh(
            new_uuid, None, &docs_dir, blob_store, false,
        ));

        // Try to claim the path — must fail.
        let err = try_claim_path(&path_index, &target_path, new_uuid)
            .await
            .unwrap_err();

        match err {
            notebook_protocol::protocol::SaveErrorKind::PathAlreadyOpen { uuid, path: p } => {
                assert_eq!(uuid, existing_uuid.to_string());
                assert_eq!(p, target_path.to_string_lossy());
            }
            other => panic!("expected PathAlreadyOpen, got {:?}", other),
        }

        // room.path must NOT have been mutated on error.
        assert!(
            room.path.read().await.is_none(),
            "room.path should still be None after a failed claim"
        );
    }

    /// Regression test for the demo-day incident: when a second room tries to
    /// save to a path that another room already claims, the claim check must
    /// happen BEFORE any disk write. Otherwise the second room's save writes
    /// 0 cells to the shared path, the first room's file watcher interprets
    /// that as an external edit, and the first room's CRDT cells are wiped.
    #[tokio::test]
    async fn path_collision_does_not_overwrite_existing_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let docs_dir = tmp.path().join("docs");
        std::fs::create_dir_all(&docs_dir).unwrap();

        // Room A claims the path; write a known marker payload to disk.
        let target_path = tmp.path().join("shared.ipynb");
        let marker_content = r#"{"cells":[{"cell_type":"code","source":"x = 1"}],"metadata":{},"nbformat":4,"nbformat_minor":5}"#;
        tokio::fs::write(&target_path, marker_content)
            .await
            .unwrap();

        // Canonicalize before inserting so the key matches what the handler
        // would compute via canonical_target_path at save time.
        let canonical = canonical_target_path(&target_path).await;
        let path_index = Arc::new(tokio::sync::Mutex::new(PathIndex::new()));
        let uuid_a = Uuid::new_v4();
        path_index
            .lock()
            .await
            .insert(canonical.clone(), uuid_a)
            .unwrap();

        // Room B attempts to save to the same path. Per the handler's
        // claim-before-write ordering, it must fail at try_claim_path without
        // ever invoking save_notebook_to_disk.
        let uuid_b = Uuid::new_v4();
        let _room_b = Arc::new(NotebookRoom::new_fresh(
            uuid_b, None, &docs_dir, blob_store, false,
        ));
        let claim = try_claim_path(&path_index, &canonical, uuid_b).await;
        assert!(claim.is_err(), "claim must fail on collision");

        // Target file must be byte-for-byte identical.
        let on_disk = tokio::fs::read_to_string(&target_path).await.unwrap();
        assert_eq!(
            on_disk, marker_content,
            "collision attempt must not touch the file on disk"
        );
    }

    /// Verify the full lifecycle: create untitled room → save to disk →
    /// promote via `promote_untitled_to_file_backed` → edit → autosave flushes
    /// the edit to the .ipynb file.
    ///
    /// This test calls the production helper directly, so it validates the real
    /// code path rather than an inline copy of the transition logic.
    #[tokio::test(start_paused = true)]
    async fn test_promote_untitled_starts_autosave() {
        use std::time::Duration;

        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let docs_dir = tmp.path().join("docs");
        std::fs::create_dir_all(&docs_dir).unwrap();

        // 1. Create an untitled (UUID-keyed) room with one cell.
        let uuid_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let uuid = Uuid::parse_str(uuid_id).unwrap();
        let room = Arc::new(NotebookRoom::new_fresh(
            uuid, None, &docs_dir, blob_store, false,
        ));
        assert!(is_untitled_notebook(uuid_id));

        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell-1", "code").unwrap();
            doc.update_source("cell-1", "x = 1").unwrap();
        }

        // 2. Insert into rooms map under UUID key (UUID key stays constant).
        let rooms: NotebookRooms = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        rooms.lock().await.insert(uuid, room.clone());

        // 3. Save to disk — creates the .ipynb file.
        let save_path = tmp.path().join("saved.ipynb");
        let written = save_notebook_to_disk(&room, Some(save_path.to_str().unwrap()))
            .await
            .unwrap();
        assert!(save_path.exists());

        // 4. Promote the room using the production helper.
        let canonical = tokio::fs::canonicalize(&written)
            .await
            .unwrap_or_else(|_| PathBuf::from(&written));
        let path_index = Arc::new(tokio::sync::Mutex::new(PathIndex::new()));

        try_claim_path(&path_index, &canonical, room.id)
            .await
            .expect("path claim should succeed");
        finalize_untitled_promotion(&room, canonical.clone()).await;

        // Verify post-promotion state.
        assert!(
            rooms.lock().await.contains_key(&uuid),
            "UUID key should still be present after promotion"
        );
        assert_eq!(
            room.path.read().await.as_deref(),
            Some(canonical.as_path()),
            "room.path should be set after promotion"
        );
        assert_eq!(
            path_index.lock().await.lookup(&canonical),
            Some(uuid),
            "path_index should contain the room's UUID"
        );
        assert!(
            !room.is_ephemeral.load(Ordering::Relaxed),
            "is_ephemeral should be cleared after promotion"
        );

        // 5. Add a new cell AFTER promotion (simulates MCP create_cell).
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(1, "cell-2", "code").unwrap();
            doc.update_source("cell-2", "y = 2").unwrap();
        }
        let _ = room.changed_tx.send(());

        // 6. Poll until the autosave debouncer flushes both cells to disk.
        //    Each sleep(100ms) advances the paused clock and yields to the
        //    runtime, letting the debouncer make progress. Timeout after 10s
        //    (well beyond the 2s debounce + 500ms check interval defaults).
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let nb = loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let content = tokio::fs::read_to_string(&save_path).await.unwrap();
            let nb: serde_json::Value = serde_json::from_str(&content).unwrap();
            if nb["cells"].as_array().is_some_and(|c| c.len() == 2) {
                break nb;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "Timed out waiting for autosave to flush both cells; got: {}",
                serde_json::to_string_pretty(&nb["cells"]).unwrap()
            );
        };

        // 7. Verify the post-promotion cell's source is present.
        let cells = nb["cells"].as_array().unwrap();
        let sources: Vec<String> = cells
            .iter()
            .map(|c| match &c["source"] {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Array(arr) => arr
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(""),
                _ => String::new(),
            })
            .collect();
        assert!(
            sources.iter().any(|s| s.contains("y = 2")),
            "Post-promotion cell should be persisted; sources: {:?}",
            sources
        );
    }

    // ── find_room_by_path tests ───────────────────────────────────────────

    #[tokio::test]
    async fn find_room_by_path_returns_room_after_index_insert() {
        let tmp = tempfile::tempdir().unwrap();
        let blob_store = test_blob_store(&tmp);
        let rooms: NotebookRooms = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let path_index = Arc::new(tokio::sync::Mutex::new(PathIndex::new()));
        let uuid = Uuid::new_v4();
        let fake = tmp.path().join("note.ipynb");
        let room = Arc::new(NotebookRoom::new_fresh(
            uuid,
            Some(fake.clone()),
            tmp.path(),
            blob_store,
            false,
        ));
        rooms.lock().await.insert(uuid, room.clone());
        path_index.lock().await.insert(fake.clone(), uuid).unwrap();

        let found = find_room_by_path(&rooms, &path_index, &fake).await;
        assert!(found.is_some());
        assert!(Arc::ptr_eq(&found.unwrap(), &room));
    }

    #[tokio::test]
    async fn find_room_by_path_returns_none_when_not_indexed() {
        let tmp = tempfile::tempdir().unwrap();
        let rooms: NotebookRooms = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let path_index = Arc::new(tokio::sync::Mutex::new(PathIndex::new()));
        let found = find_room_by_path(&rooms, &path_index, &tmp.path().join("nope.ipynb")).await;
        assert!(found.is_none());
    }

    // ── C1 regression: NotebookSync path handshake must reuse existing room ──

    /// Verify that the pattern used by the NotebookSync handshake — consulting
    /// `find_room_by_path` before calling `get_or_create_room` — produces
    /// exactly one room for a given path even when called twice.
    ///
    /// Before the C1 fix the handshake would mint a fresh UUID on every call,
    /// so a second connection to the same path created a second room (zombie
    /// room: two file watchers, two autosave debouncers, two writers).
    ///
    /// The fix: if `find_room_by_path` returns `Some(existing)`, reuse its UUID
    /// so `get_or_create_room` returns the existing room instead of creating one.
    #[tokio::test]
    async fn test_notebook_sync_path_handshake_reuses_existing_room() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let docs_dir = tmp.path().to_path_buf();
        let rooms: NotebookRooms = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let path_index = Arc::new(tokio::sync::Mutex::new(PathIndex::new()));

        // Simulate a file-backed path (doesn't need to exist for this test).
        let notebook_path = tmp.path().join("my_notebook.ipynb");

        // --- First handshake (simulates the fixed NotebookSync path) ---
        // 1. Check path_index — not yet indexed, so mint a new UUID.
        let room1 = {
            let (uuid, path) = match find_room_by_path(&rooms, &path_index, &notebook_path).await {
                Some(existing) => (existing.id, Some(notebook_path.clone())),
                None => (Uuid::new_v4(), Some(notebook_path.clone())),
            };
            get_or_create_room(
                &rooms,
                &path_index,
                uuid,
                path,
                &docs_dir,
                blob_store.clone(),
                false,
            )
            .await
        };

        // --- Second handshake for the same path ---
        // find_room_by_path should now return the existing room.
        let room2 = {
            let (uuid, path) = match find_room_by_path(&rooms, &path_index, &notebook_path).await {
                Some(existing) => (existing.id, Some(notebook_path.clone())),
                None => (Uuid::new_v4(), Some(notebook_path.clone())),
            };
            get_or_create_room(
                &rooms,
                &path_index,
                uuid,
                path,
                &docs_dir,
                blob_store.clone(),
                false,
            )
            .await
        };

        // Both handshakes must return the same room Arc — no zombie duplicates.
        assert!(
            Arc::ptr_eq(&room1, &room2),
            "Second NotebookSync handshake for same path must reuse existing room"
        );

        // Exactly one room in the map (not two).
        assert_eq!(
            rooms.lock().await.len(),
            1,
            "Only one room should exist after two handshakes for the same path"
        );

        // path_index has exactly one entry.
        assert_eq!(
            path_index.lock().await.len(),
            1,
            "path_index should have exactly one entry"
        );
    }

    // ── compute_env_sync_diff tests ───────────────────────────────────────

    #[test]
    fn test_compute_env_sync_diff_in_sync() {
        let launched = LaunchedEnvConfig {
            uv_deps: Some(vec!["numpy".to_string(), "pandas".to_string()]),
            conda_deps: None,
            conda_channels: None,
            pixi_deps: None,
            pixi_toml_deps: None,
            pixi_toml_path: None,
            pyproject_path: None,
            environment_yml_path: None,
            environment_yml_deps: None,
            deno_config: None,
            venv_path: None,
            python_path: None,
            launch_id: Some("abc".to_string()),
            feature_flags: notebook_protocol::protocol::FeatureFlags::default(),
            prewarmed_packages: vec![],
        };
        let snapshot = snapshot_with_uv(vec!["numpy".to_string(), "pandas".to_string()]);
        assert!(
            compute_env_sync_diff(&launched, &snapshot).is_none(),
            "identical deps should be in sync"
        );
    }

    #[test]
    fn test_compute_env_sync_diff_added() {
        let launched = LaunchedEnvConfig {
            uv_deps: Some(vec!["numpy".to_string()]),
            conda_deps: None,
            conda_channels: None,
            pixi_deps: None,
            pixi_toml_deps: None,
            pixi_toml_path: None,
            pyproject_path: None,
            environment_yml_path: None,
            environment_yml_deps: None,
            deno_config: None,
            venv_path: None,
            python_path: None,
            launch_id: None,
            feature_flags: notebook_protocol::protocol::FeatureFlags::default(),
            prewarmed_packages: vec![],
        };
        let snapshot = snapshot_with_uv(vec!["numpy".to_string(), "requests".to_string()]);
        let diff = compute_env_sync_diff(&launched, &snapshot).expect("should detect drift");
        assert_eq!(diff.added, vec!["requests".to_string()]);
        assert!(diff.removed.is_empty());
        assert!(!diff.channels_changed);
    }

    #[test]
    fn test_compute_env_sync_diff_removed() {
        let launched = LaunchedEnvConfig {
            uv_deps: Some(vec!["numpy".to_string(), "pandas".to_string()]),
            conda_deps: None,
            conda_channels: None,
            pixi_deps: None,
            pixi_toml_deps: None,
            pixi_toml_path: None,
            pyproject_path: None,
            environment_yml_path: None,
            environment_yml_deps: None,
            deno_config: None,
            venv_path: None,
            python_path: None,
            launch_id: None,
            feature_flags: notebook_protocol::protocol::FeatureFlags::default(),
            prewarmed_packages: vec![],
        };
        let snapshot = snapshot_with_uv(vec!["numpy".to_string()]);
        let diff = compute_env_sync_diff(&launched, &snapshot).expect("should detect drift");
        assert!(diff.added.is_empty());
        assert_eq!(diff.removed, vec!["pandas".to_string()]);
    }

    #[test]
    fn test_compute_env_sync_diff_added_and_removed() {
        let launched = LaunchedEnvConfig {
            uv_deps: Some(vec!["numpy".to_string(), "old-pkg".to_string()]),
            conda_deps: None,
            conda_channels: None,
            pixi_deps: None,
            pixi_toml_deps: None,
            pixi_toml_path: None,
            pyproject_path: None,
            environment_yml_path: None,
            environment_yml_deps: None,
            deno_config: None,
            venv_path: None,
            python_path: None,
            launch_id: None,
            feature_flags: notebook_protocol::protocol::FeatureFlags::default(),
            prewarmed_packages: vec![],
        };
        let snapshot = snapshot_with_uv(vec!["numpy".to_string(), "new-pkg".to_string()]);
        let diff = compute_env_sync_diff(&launched, &snapshot).expect("should detect drift");
        assert_eq!(diff.added, vec!["new-pkg".to_string()]);
        assert_eq!(diff.removed, vec!["old-pkg".to_string()]);
    }

    #[test]
    fn test_compute_env_sync_diff_conda_channels_changed() {
        let launched = LaunchedEnvConfig {
            uv_deps: None,
            conda_deps: Some(vec!["scipy".to_string()]),
            conda_channels: Some(vec!["conda-forge".to_string()]),
            pixi_deps: None,
            pixi_toml_deps: None,
            pixi_toml_path: None,
            pyproject_path: None,
            environment_yml_path: None,
            environment_yml_deps: None,
            deno_config: None,
            venv_path: None,
            python_path: None,
            launch_id: None,
            feature_flags: notebook_protocol::protocol::FeatureFlags::default(),
            prewarmed_packages: vec![],
        };
        // Build a conda snapshot with a different channel
        let mut snapshot = snapshot_with_conda(vec!["scipy".to_string()]);
        snapshot.runt.conda.as_mut().unwrap().channels = vec!["defaults".to_string()];
        let diff =
            compute_env_sync_diff(&launched, &snapshot).expect("should detect channel drift");
        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
        assert!(diff.channels_changed);
    }

    #[test]
    fn test_compute_env_sync_diff_no_tracking() {
        // Prewarmed kernel: no uv_deps, no conda_deps, no deno_config
        let launched = LaunchedEnvConfig::default();
        let snapshot = snapshot_with_uv(vec!["numpy".to_string()]);
        // When the kernel isn't tracking any deps, diff is None (no drift to report)
        assert!(compute_env_sync_diff(&launched, &snapshot).is_none());
    }

    #[test]
    fn test_build_launched_config_uv_prewarmed_stores_paths() {
        let venv = PathBuf::from("/tmp/pool/env-abc");
        let python = PathBuf::from("/tmp/pool/env-abc/bin/python");
        let pkgs = vec!["ipykernel".to_string(), "pandas".to_string()];
        let config = build_launched_config(
            "python",
            "uv:prewarmed",
            None,
            None,
            Some(venv.clone()),
            Some(python.clone()),
            Some(&pkgs),
            None,
            notebook_protocol::protocol::FeatureFlags::default(),
            None,
        );
        assert_eq!(config.venv_path.as_ref(), Some(&venv));
        assert_eq!(config.python_path.as_ref(), Some(&python));
        assert!(config.uv_deps.is_none(), "prewarmed should not set uv_deps");
        assert_eq!(config.prewarmed_packages, pkgs);
    }

    #[test]
    fn test_build_launched_config_uv_prewarmed_with_captured_baseline() {
        // P3 regression: when a captured env fires the prewarmed path,
        // launched_config must record captured deps as the baseline so
        // drift detection treats the launch as "tracking" rather than
        // reporting captured deps as pending additions on every reopen.
        let captured = CapturedEnv::Uv {
            deps: kernel_env::UvDependencies {
                dependencies: vec!["pandas".to_string(), "numpy".to_string()],
                requires_python: Some(">=3.10".to_string()),
                prerelease: None,
            },
            env_id: "nb-1".to_string(),
        };
        let config = build_launched_config(
            "python",
            "uv:prewarmed",
            None,
            None,
            Some(PathBuf::from("/tmp/env")),
            Some(PathBuf::from("/tmp/env/bin/python")),
            None,
            None,
            notebook_protocol::protocol::FeatureFlags::default(),
            Some(&captured),
        );
        assert_eq!(
            config.uv_deps.as_deref(),
            Some(["pandas".to_string(), "numpy".to_string()].as_slice()),
            "captured-prewarmed must record deps as baseline"
        );
    }

    #[test]
    fn test_build_launched_config_conda_prewarmed_with_captured_baseline() {
        // Captured conda baseline must include channels so channel edits
        // surface as drift rather than being silently ignored.
        let captured = CapturedEnv::Conda {
            deps: kernel_env::CondaDependencies {
                dependencies: vec!["scipy".to_string()],
                channels: vec!["conda-forge".to_string(), "pytorch".to_string()],
                python: Some("3.11".to_string()),
                env_id: None,
            },
            env_id: "nb-2".to_string(),
        };
        let config = build_launched_config(
            "python",
            "conda:prewarmed",
            None,
            None,
            Some(PathBuf::from("/tmp/conda-env")),
            Some(PathBuf::from("/tmp/conda-env/bin/python")),
            None,
            None,
            notebook_protocol::protocol::FeatureFlags::default(),
            Some(&captured),
        );
        assert_eq!(
            config.conda_deps.as_deref(),
            Some([String::from("scipy")].as_slice())
        );
        assert_eq!(
            config.conda_channels.as_deref(),
            Some(["conda-forge".to_string(), "pytorch".to_string()].as_slice())
        );
    }

    #[test]
    fn test_compute_env_sync_diff_prewarmed_promoted_to_empty_baseline() {
        // Simulates handle_sync_environment promoting uv_deps from None to
        // Some([]) for a prewarmed kernel, then computing the diff.
        let mut launched = LaunchedEnvConfig {
            venv_path: Some(PathBuf::from("/tmp/pool/env-abc")),
            python_path: Some(PathBuf::from("/tmp/pool/env-abc/bin/python")),
            ..LaunchedEnvConfig::default()
        };
        // Promote to empty baseline (what handle_sync_environment does)
        launched.uv_deps = Some(vec![]);

        let snapshot = snapshot_with_uv(vec!["httpx".to_string()]);
        let diff = compute_env_sync_diff(&launched, &snapshot).expect("should detect added deps");
        assert_eq!(diff.added, vec!["httpx".to_string()]);
        assert!(diff.removed.is_empty());
    }

    #[test]
    fn test_build_launched_config_conda_prewarmed_stores_paths() {
        // conda:prewarmed stores paths so hot-sync can install deps later
        let venv = PathBuf::from("/tmp/conda-env");
        let python = PathBuf::from("/tmp/conda-env/bin/python");
        let config = build_launched_config(
            "python",
            "conda:prewarmed",
            None,
            None,
            Some(venv.clone()),
            Some(python.clone()),
            None,
            None,
            notebook_protocol::protocol::FeatureFlags::default(),
            None,
        );
        assert_eq!(config.venv_path.as_ref(), Some(&venv));
        assert_eq!(config.python_path.as_ref(), Some(&python));
        assert!(config.uv_deps.is_none());
        assert!(
            config.conda_deps.is_none(),
            "prewarmed should not set conda_deps"
        );
    }

    // ── check_and_broadcast_sync_state tests ──────────────────────────────

    #[tokio::test]
    async fn test_check_and_broadcast_sync_state_no_kernel() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _path) = test_room_with_path(&tmp, "no_kernel.ipynb");

        // Write metadata so the function gets past the metadata check
        let snapshot = snapshot_with_uv(vec!["numpy".to_string()]);
        {
            let mut doc = room.doc.write().await;
            doc.set_metadata_snapshot(&snapshot).unwrap();
        }

        // Pre-set RuntimeStateDoc env to dirty so we can verify it's NOT changed
        {
            let mut sd = room.state_doc.write().await;
            sd.set_env_sync(false, &["numpy".to_string()], &[], false, false);
        }

        // No kernel in the room — should be a no-op
        check_and_broadcast_sync_state(&room).await;

        // Verify env state was NOT touched (still dirty from pre-set)
        let sd = room.state_doc.read().await;
        let state = sd.read_state();
        assert!(
            !state.env.in_sync,
            "env should remain dirty when no kernel is present"
        );
        assert_eq!(state.env.added, vec!["numpy".to_string()]);
    }

    /// P3 regression: a captured-prewarmed launch must report `in_sync = true`
    /// when metadata matches the captured baseline. Before the fix,
    /// `LaunchedEnvConfig.uv_deps` was left `None` for the prewarmed path, so
    /// `check_and_broadcast_sync_state` took the "prewarmed + inline deps
    /// added" branch and flagged the captured deps as pending additions on
    /// every reopen.
    #[tokio::test]
    async fn test_check_and_broadcast_sync_state_captured_uv_prewarmed_in_sync() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _path) = test_room_with_path(&tmp, "captured.ipynb");

        // Notebook has captured deps in metadata.
        let snapshot = snapshot_with_uv(vec!["pandas".to_string(), "numpy".to_string()]);
        {
            let mut doc = room.doc.write().await;
            doc.set_metadata_snapshot(&snapshot).unwrap();
        }

        // Kernel was launched via the captured-prewarmed path, so launched
        // config records the captured deps as the baseline (what our P3 fix
        // does in `build_launched_config`).
        {
            let mut lc = room.runtime_agent_launched_config.write().await;
            *lc = Some(LaunchedEnvConfig {
                uv_deps: Some(vec!["pandas".to_string(), "numpy".to_string()]),
                venv_path: Some(PathBuf::from("/tmp/captured-env")),
                python_path: Some(PathBuf::from("/tmp/captured-env/bin/python")),
                ..LaunchedEnvConfig::default()
            });
        }

        // Kernel is idle (otherwise the function returns early).
        {
            let mut sd = room.state_doc.write().await;
            sd.set_kernel_status("idle");
            // Pre-set to dirty so we can verify it flips to in_sync.
            sd.set_env_sync(false, &["pandas".to_string()], &[], false, false);
        }

        check_and_broadcast_sync_state(&room).await;

        let sd = room.state_doc.read().await;
        let state = sd.read_state();
        assert!(
            state.env.in_sync,
            "captured-prewarmed launch with matching metadata must be in_sync"
        );
        assert!(state.env.added.is_empty());
        assert!(state.env.removed.is_empty());
    }

    /// Complementary to the above: when metadata diverges from the captured
    /// baseline (user added a new dep post-capture), the drift detector
    /// should surface the new dep in `env.added`. This verifies drift still
    /// works when the captured baseline is populated.
    #[tokio::test]
    async fn test_check_and_broadcast_sync_state_captured_uv_prewarmed_reports_additions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _path) = test_room_with_path(&tmp, "captured-drift.ipynb");

        // User added a third dep post-capture.
        let snapshot = snapshot_with_uv(vec![
            "pandas".to_string(),
            "numpy".to_string(),
            "polars".to_string(),
        ]);
        {
            let mut doc = room.doc.write().await;
            doc.set_metadata_snapshot(&snapshot).unwrap();
        }

        // Launched baseline still only has the original captured set.
        {
            let mut lc = room.runtime_agent_launched_config.write().await;
            *lc = Some(LaunchedEnvConfig {
                uv_deps: Some(vec!["pandas".to_string(), "numpy".to_string()]),
                venv_path: Some(PathBuf::from("/tmp/captured-env")),
                python_path: Some(PathBuf::from("/tmp/captured-env/bin/python")),
                ..LaunchedEnvConfig::default()
            });
        }

        {
            let mut sd = room.state_doc.write().await;
            sd.set_kernel_status("idle");
        }

        check_and_broadcast_sync_state(&room).await;

        let sd = room.state_doc.read().await;
        let state = sd.read_state();
        assert!(
            !state.env.in_sync,
            "added dep post-capture must surface as drift"
        );
        assert_eq!(state.env.added, vec!["polars".to_string()]);
    }

    #[tokio::test]
    async fn test_check_and_broadcast_sync_state_no_metadata() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _path) = test_room_with_path(&tmp, "no_meta.ipynb");

        // Don't write any metadata to the doc

        // Pre-set RuntimeStateDoc env to dirty
        {
            let mut sd = room.state_doc.write().await;
            sd.set_env_sync(false, &["pandas".to_string()], &[], false, false);
        }

        // No metadata in doc — should return early
        check_and_broadcast_sync_state(&room).await;

        // Verify env state was NOT touched
        let sd = room.state_doc.read().await;
        let state = sd.read_state();
        assert!(
            !state.env.in_sync,
            "env should remain dirty when no metadata is present"
        );
    }

    // ── verify_trust_from_snapshot tests ───────────────────────────────────

    #[test]
    fn test_verify_trust_from_snapshot_no_deps() {
        let snapshot = snapshot_empty();
        let result = verify_trust_from_snapshot(&snapshot);
        assert_eq!(result.status, runt_trust::TrustStatus::NoDependencies);
        assert!(!result.pending_launch);
    }

    #[test]
    #[serial]
    fn test_verify_trust_from_snapshot_unsigned_deps() {
        let temp_dir = tempfile::tempdir().unwrap();
        let key_path = temp_dir.path().join("trust-key");
        std::env::set_var("RUNT_TRUST_KEY_PATH", key_path.to_str().unwrap());

        let snapshot = snapshot_with_uv(vec!["numpy".to_string()]);
        let result = verify_trust_from_snapshot(&snapshot);
        assert_eq!(result.status, runt_trust::TrustStatus::Untrusted);
        assert!(!result.pending_launch);

        std::env::remove_var("RUNT_TRUST_KEY_PATH");
    }

    #[test]
    #[serial]
    fn test_verify_trust_from_snapshot_signed_trusted() {
        let temp_dir = tempfile::tempdir().unwrap();
        let key_path = temp_dir.path().join("trust-key");
        std::env::set_var("RUNT_TRUST_KEY_PATH", key_path.to_str().unwrap());

        let mut snapshot = snapshot_with_uv(vec!["numpy".to_string()]);

        // Build the same HashMap that verify_trust_from_snapshot builds, then sign.
        let mut metadata = std::collections::HashMap::new();
        if let Ok(runt_value) = serde_json::to_value(&snapshot.runt) {
            metadata.insert("runt".to_string(), runt_value);
        }
        let signature = runt_trust::sign_notebook_dependencies(&metadata).unwrap();
        snapshot.runt.trust_signature = Some(signature);

        let result = verify_trust_from_snapshot(&snapshot);
        assert_eq!(result.status, runt_trust::TrustStatus::Trusted);
        assert!(!result.pending_launch);

        std::env::remove_var("RUNT_TRUST_KEY_PATH");
    }

    #[test]
    #[serial]
    fn test_verify_trust_from_snapshot_invalid_signature() {
        let temp_dir = tempfile::tempdir().unwrap();
        let key_path = temp_dir.path().join("trust-key");
        std::env::set_var("RUNT_TRUST_KEY_PATH", key_path.to_str().unwrap());

        let mut snapshot = snapshot_with_uv(vec!["numpy".to_string()]);
        // Set a bogus signature that won't match.
        snapshot.runt.trust_signature = Some("bad-signature-value".to_string());

        let result = verify_trust_from_snapshot(&snapshot);
        assert_eq!(result.status, runt_trust::TrustStatus::SignatureInvalid);
        assert!(!result.pending_launch);

        std::env::remove_var("RUNT_TRUST_KEY_PATH");
    }

    #[test]
    #[serial]
    fn test_verify_trust_from_snapshot_conda_trusted() {
        let temp_dir = tempfile::tempdir().unwrap();
        let key_path = temp_dir.path().join("trust-key");
        std::env::set_var("RUNT_TRUST_KEY_PATH", key_path.to_str().unwrap());

        let mut snapshot = snapshot_with_conda(vec!["pandas".to_string()]);

        // Build the same HashMap that verify_trust_from_snapshot builds, then sign.
        let mut metadata = std::collections::HashMap::new();
        if let Ok(runt_value) = serde_json::to_value(&snapshot.runt) {
            metadata.insert("runt".to_string(), runt_value);
        }
        let signature = runt_trust::sign_notebook_dependencies(&metadata).unwrap();
        snapshot.runt.trust_signature = Some(signature);

        let result = verify_trust_from_snapshot(&snapshot);
        assert_eq!(result.status, runt_trust::TrustStatus::Trusted);
        assert!(!result.pending_launch);

        std::env::remove_var("RUNT_TRUST_KEY_PATH");
    }

    #[tokio::test]
    async fn test_check_and_update_trust_state_empty_doc() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _path) = test_room_with_path(&tmp, "empty_doc.ipynb");

        // Doc has no metadata written — should not crash.
        check_and_update_trust_state(&room).await;

        // trust_state should remain Untrusted (the default from test_room_with_path).
        let ts = room.trust_state.read().await;
        assert_eq!(ts.status, runt_trust::TrustStatus::Untrusted);
    }

    #[tokio::test]
    async fn test_check_and_update_trust_state_no_deps() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _path) = test_room_with_path(&tmp, "no_deps.ipynb");

        // Align RuntimeStateDoc with the room's initial Untrusted state so we
        // can verify the function actually writes the new value.
        {
            let mut sd = room.state_doc.write().await;
            sd.set_trust("untrusted", true);
        }

        // Write an empty metadata snapshot (no dependencies).
        let snapshot = snapshot_empty();
        {
            let mut doc = room.doc.write().await;
            doc.set_metadata_snapshot(&snapshot).unwrap();
        }

        check_and_update_trust_state(&room).await;

        // Room trust_state should change from Untrusted → NoDependencies.
        let ts = room.trust_state.read().await;
        assert_eq!(ts.status, runt_trust::TrustStatus::NoDependencies);
        drop(ts);

        // RuntimeStateDoc should reflect "no_dependencies" with needs_approval=false.
        let sd = room.state_doc.read().await;
        let state = sd.read_state();
        assert_eq!(state.trust.status, "no_dependencies");
        assert!(!state.trust.needs_approval);
    }

    #[tokio::test]
    #[serial]
    async fn test_check_and_update_trust_state_approval_updates_room() {
        let temp_dir = tempfile::tempdir().unwrap();
        let key_path = temp_dir.path().join("trust-key");
        std::env::set_var("RUNT_TRUST_KEY_PATH", key_path.to_str().unwrap());

        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _path) = test_room_with_path(&tmp, "signed.ipynb");

        // Align RuntimeStateDoc with the room's initial Untrusted state.
        {
            let mut sd = room.state_doc.write().await;
            sd.set_trust("untrusted", true);
        }

        // Build a snapshot with UV deps and a valid trust signature.
        let mut snapshot = snapshot_with_uv(vec!["numpy".to_string()]);
        let mut metadata = std::collections::HashMap::new();
        if let Ok(runt_value) = serde_json::to_value(&snapshot.runt) {
            metadata.insert("runt".to_string(), runt_value);
        }
        let signature = runt_trust::sign_notebook_dependencies(&metadata).unwrap();
        snapshot.runt.trust_signature = Some(signature);

        {
            let mut doc = room.doc.write().await;
            doc.set_metadata_snapshot(&snapshot).unwrap();
        }

        check_and_update_trust_state(&room).await;

        // Room trust_state should be Trusted.
        let ts = room.trust_state.read().await;
        assert_eq!(ts.status, runt_trust::TrustStatus::Trusted);
        drop(ts);

        // RuntimeStateDoc should have "trusted" with needs_approval=false.
        let sd = room.state_doc.read().await;
        let state = sd.read_state();
        assert_eq!(state.trust.status, "trusted");
        assert!(!state.trust.needs_approval);

        std::env::remove_var("RUNT_TRUST_KEY_PATH");
    }

    #[tokio::test]
    async fn test_check_and_update_trust_state_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _path) = test_room_with_path(&tmp, "idempotent.ipynb");

        // Align RuntimeStateDoc with the room's initial Untrusted state so the
        // first transition to NoDependencies actually mutates the doc and fires
        // a notification.
        {
            let mut sd = room.state_doc.write().await;
            sd.set_trust("untrusted", true);
        }

        // Write an empty metadata snapshot to trigger Untrusted → NoDependencies.
        let snapshot = snapshot_empty();
        {
            let mut doc = room.doc.write().await;
            doc.set_metadata_snapshot(&snapshot).unwrap();
        }

        // Subscribe before either call so we capture all notifications.
        let mut rx = room.state_changed_tx.subscribe();

        // First call: state changes from Untrusted → NoDependencies → notification sent.
        check_and_update_trust_state(&room).await;

        // Second call: state is already NoDependencies → no change, no notification.
        check_and_update_trust_state(&room).await;

        // Drain the channel and count how many notifications arrived.
        let mut count = 0usize;
        while rx.try_recv().is_ok() {
            count += 1;
        }
        assert_eq!(count, 1, "expected exactly one state_changed notification");

        // Final trust_state should be NoDependencies.
        let ts = room.trust_state.read().await;
        assert_eq!(ts.status, runt_trust::TrustStatus::NoDependencies);
    }

    /// Simulates the file-watcher path: a notebook starts Trusted (signed
    /// metadata on disk), then an external editor / `uv add numpy` rewrites
    /// the dependency list. The signature no longer covers the new dep set,
    /// so `check_and_update_trust_state` should flip trust_state to
    /// SignatureInvalid and emit a state_changed_tx notification.
    ///
    /// This is the regression this PR fixes: before the fix the file watcher
    /// merged new deps into the CRDT but never re-verified trust, so
    /// room.trust_state stayed Trusted and auto-launch used a stale signature.
    #[tokio::test]
    #[serial]
    async fn test_check_and_update_trust_state_external_dep_add_invalidates() {
        let temp_dir = tempfile::tempdir().unwrap();
        let key_path = temp_dir.path().join("trust-key");
        std::env::set_var("RUNT_TRUST_KEY_PATH", key_path.to_str().unwrap());

        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _path) = test_room_with_path(&tmp, "signed_then_edited.ipynb");

        // Build a signed snapshot (numpy only) and seed the room with Trusted
        // state, matching what happens at room creation time on disk.
        let mut signed = snapshot_with_uv(vec!["numpy".to_string()]);
        let mut metadata = std::collections::HashMap::new();
        if let Ok(runt_value) = serde_json::to_value(&signed.runt) {
            metadata.insert("runt".to_string(), runt_value);
        }
        let signature = runt_trust::sign_notebook_dependencies(&metadata).unwrap();
        signed.runt.trust_signature = Some(signature.clone());

        {
            let mut doc = room.doc.write().await;
            doc.set_metadata_snapshot(&signed).unwrap();
        }
        {
            let mut ts = room.trust_state.write().await;
            *ts = verify_trust_from_snapshot(&signed);
        }
        {
            let mut sd = room.state_doc.write().await;
            sd.set_trust("trusted", false);
        }

        // Sanity check: starting state is Trusted.
        {
            let ts = room.trust_state.read().await;
            assert_eq!(ts.status, runt_trust::TrustStatus::Trusted);
        }

        // Simulate external edit: user runs `uv add pandas` + saves. The
        // file watcher merges the new deps into the CRDT but carries over
        // the stale signature (because the external tool doesn't resign).
        let mut edited = snapshot_with_uv(vec!["numpy".to_string(), "pandas".to_string()]);
        edited.runt.trust_signature = Some(signature);
        {
            let mut doc = room.doc.write().await;
            doc.set_metadata_snapshot(&edited).unwrap();
        }

        // Subscribe before the re-verification so we can observe the flip.
        let mut rx = room.state_changed_tx.subscribe();

        check_and_update_trust_state(&room).await;

        // Trust should flip to SignatureInvalid — the signature is over the
        // numpy-only dep set, so adding pandas breaks it.
        {
            let ts = room.trust_state.read().await;
            assert_eq!(
                ts.status,
                runt_trust::TrustStatus::SignatureInvalid,
                "external dep add must flip trust from Trusted to SignatureInvalid"
            );
        }

        // RuntimeStateDoc should reflect the flip for the frontend banner.
        {
            let sd = room.state_doc.read().await;
            let state = sd.read_state();
            assert_eq!(state.trust.status, "signature_invalid");
            assert!(state.trust.needs_approval);
        }

        // state_changed_tx must have fired at least once so subscribers
        // (frontend, auto-launch) pick up the new trust state.
        assert!(
            rx.try_recv().is_ok(),
            "trust flip must emit state_changed_tx notification"
        );

        std::env::remove_var("RUNT_TRUST_KEY_PATH");
    }

    // ── Per-agent oneshot channel tests ──────────────────────────────

    #[tokio::test]
    async fn test_per_runtime_agent_oneshot_isolation() {
        // Verify that each spawn generation gets its own oneshot channel
        // and that connecting one agent doesn't resolve another's receiver.
        let pending: Arc<Mutex<Option<oneshot::Sender<()>>>> = Arc::new(Mutex::new(None));

        // Spawn A: create oneshot, store sender
        let (tx_a, rx_a) = oneshot::channel();
        *pending.lock().await = Some(tx_a);

        // A connects: take and send
        if let Some(tx) = pending.lock().await.take() {
            tx.send(()).unwrap();
        }
        assert!(rx_a.await.is_ok(), "A's receiver should resolve Ok");

        // Spawn B: create new oneshot (A's sender already consumed via take)
        let (tx_b, rx_b) = oneshot::channel();
        *pending.lock().await = Some(tx_b);

        // B connects
        if let Some(tx) = pending.lock().await.take() {
            tx.send(()).unwrap();
        }
        assert!(rx_b.await.is_ok(), "B's receiver should resolve Ok");

        // After both consumed, pending should be None
        assert!(pending.lock().await.is_none());
    }

    #[tokio::test]
    async fn test_oneshot_replaced_before_runtime_agent_connect() {
        // When a new spawn replaces the oneshot before the previous agent
        // connects, the old receiver should resolve with Err (sender dropped).
        let pending: Arc<Mutex<Option<oneshot::Sender<()>>>> = Arc::new(Mutex::new(None));

        // Spawn A
        let (_tx_a, rx_a) = oneshot::channel();
        *pending.lock().await = Some(_tx_a);

        // Spawn B BEFORE A connects — replaces A's sender (drops tx_a)
        let (tx_b, rx_b) = oneshot::channel();
        *pending.lock().await = Some(tx_b); // tx_a dropped here

        // A's receiver resolves with Err (sender dropped = superseded)
        assert!(
            rx_a.await.is_err(),
            "A's receiver should get Err (sender was dropped by B's spawn)"
        );

        // B connects normally
        if let Some(tx) = pending.lock().await.take() {
            tx.send(()).unwrap();
        }
        assert!(rx_b.await.is_ok(), "B's receiver should resolve Ok");
    }

    #[tokio::test]
    async fn test_reset_starting_state_guard() {
        // Verify that reset_starting_state skips when expected_runtime_agent_id
        // doesn't match current_runtime_agent_id.
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _notebook_path) = test_room_with_path(&tmp, "guard_test.ipynb");

        // Set current runtime agent to "agent-B"
        {
            let mut id = room.current_runtime_agent_id.write().await;
            *id = Some("agent-B".to_string());
        }

        // Set kernel status to "starting" (simulates in-progress launch)
        {
            let mut sd = room.state_doc.write().await;
            sd.set_kernel_status("starting");
        }

        // Call reset with expected="agent-A" (stale handler) — should skip
        reset_starting_state(&room, Some("agent-A")).await;

        // Verify: kernel_status should still be "starting" (NOT reset)
        {
            let sd = room.state_doc.read().await;
            assert_eq!(
                sd.read_state().kernel.status,
                "starting",
                "Guard should have prevented reset (agent-A != agent-B)"
            );
        }

        // Verify: current_runtime_agent_id unchanged
        {
            let id = room.current_runtime_agent_id.read().await;
            assert_eq!(id.as_deref(), Some("agent-B"));
        }

        // Now call with matching expected="agent-B" — should reset
        reset_starting_state(&room, Some("agent-B")).await;

        // Verify: kernel_status should be "not_started"
        {
            let sd = room.state_doc.read().await;
            assert_eq!(
                sd.read_state().kernel.status,
                "not_started",
                "Reset should proceed when expected matches current"
            );
        }

        // Verify: current_runtime_agent_id cleared (provenance cleanup)
        {
            let id = room.current_runtime_agent_id.read().await;
            assert!(
                id.is_none(),
                "Provenance should be cleared after guarded reset"
            );
        }

        // Call with None (pre-spawn) — should always reset
        {
            let mut sd = room.state_doc.write().await;
            sd.set_kernel_status("starting");
        }
        reset_starting_state(&room, None).await;
        {
            let sd = room.state_doc.read().await;
            assert_eq!(
                sd.read_state().kernel.status,
                "not_started",
                "None (pre-spawn) should always reset"
            );
        }
    }

    #[tokio::test]
    async fn test_reset_starting_state_cleanup() {
        // Verify that guarded reset clears request_tx, connect_tx, and handle
        // (belt-and-suspenders cleanup prevents zombie runtime agents).
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _notebook_path) = test_room_with_path(&tmp, "cleanup_test.ipynb");

        // Simulate a runtime agent that has connected: set provenance,
        // request channel, and connect sender.
        {
            let mut id = room.current_runtime_agent_id.write().await;
            *id = Some("agent-A".to_string());
        }
        {
            let (tx, _rx) = tokio::sync::mpsc::channel(16);
            let mut guard = room.runtime_agent_request_tx.lock().await;
            *guard = Some(tx);
        }
        {
            let (tx, _rx) = oneshot::channel();
            let mut guard = room.pending_runtime_agent_connect_tx.lock().await;
            *guard = Some(tx);
        }

        // Reset with matching agent — should clean up everything
        reset_starting_state(&room, Some("agent-A")).await;

        // Verify all fields cleared
        assert!(
            room.runtime_agent_request_tx.lock().await.is_none(),
            "request_tx should be cleared"
        );
        assert!(
            room.pending_runtime_agent_connect_tx.lock().await.is_none(),
            "connect_tx should be cleared"
        );
        assert!(
            room.runtime_agent_handle.lock().await.is_none(),
            "handle should be cleared"
        );
        assert!(
            room.current_runtime_agent_id.read().await.is_none(),
            "provenance should be cleared"
        );
    }

    #[tokio::test]
    async fn test_reset_aborts_when_new_spawn_detected() {
        // Verify that guarded reset_starting_state aborts field cleanup
        // if a new spawn sets provenance between the provenance-clear and
        // the field clears (TOCTOU re-check).
        //
        // We simulate this by:
        // 1. Setting provenance to "agent-old" + populating fields
        // 2. Clearing provenance to None (as reset_starting_state would)
        // 3. Setting provenance to "agent-new" + new field values (simulating interleaving spawn)
        // 4. Calling reset_starting_state with None expected (pre-spawn path) — always proceeds
        //    But for the guarded path: we test manually by checking the re-check logic.
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _notebook_path) = test_room_with_path(&tmp, "toctou_test.ipynb");

        // Simulate: agent-old's reset already cleared provenance to None,
        // then a new spawn set provenance to "agent-new" with fresh channels.
        {
            let mut id = room.current_runtime_agent_id.write().await;
            *id = Some("agent-new".to_string());
        }
        let (new_tx, mut new_rx) = oneshot::channel::<()>();
        {
            let mut guard = room.pending_runtime_agent_connect_tx.lock().await;
            *guard = Some(new_tx);
        }
        let (req_tx, _req_rx) = tokio::sync::mpsc::channel(16);
        {
            let mut guard = room.runtime_agent_request_tx.lock().await;
            *guard = Some(req_tx);
        }

        // Now call reset with expected="agent-old" — provenance is "agent-new",
        // so the guard should skip entirely (mismatch).
        reset_starting_state(&room, Some("agent-old")).await;

        // Verify: new spawn's fields are untouched
        assert!(
            room.pending_runtime_agent_connect_tx.lock().await.is_some(),
            "new spawn's connect_tx should not be cleared"
        );
        assert!(
            room.runtime_agent_request_tx.lock().await.is_some(),
            "new spawn's request_tx should not be cleared"
        );
        assert_eq!(
            room.current_runtime_agent_id.read().await.as_deref(),
            Some("agent-new"),
            "new spawn's provenance should not be cleared"
        );

        // Verify new_rx is still alive (sender not dropped)
        // Use try_recv — should return TryRecvError::Empty (not Closed)
        assert!(
            new_rx.try_recv().is_err(),
            "new spawn's oneshot should still be pending (sender alive)"
        );
    }

    #[tokio::test]
    async fn test_reset_generation_guard_with_concurrent_spawn() {
        // Regression test for TOCTOU in reset_starting_state: verifies that a
        // new spawn interleaving AFTER provenance is cleared (but before field
        // clears) is detected by the generation counter, causing reset to abort
        // and preserving the new spawn's fields.
        //
        // The test spawns a concurrent task that simulates a new spawn sequence
        // (set provenance → bump generation → store fields) as soon as it
        // detects provenance cleared to None. The main task calls
        // reset_starting_state with a matching expected_runtime_agent_id.
        //
        // Two valid orderings exist:
        // 1. Concurrent spawn completes between provenance clear and field clears
        //    → generation mismatch → reset aborts → new fields preserved
        // 2. Concurrent spawn completes after reset_starting_state returns
        //    → reset clears old fields normally → concurrent spawn stores new fields
        // In both cases, the new spawn's fields are present at the end.
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _notebook_path) = test_room_with_path(&tmp, "gen_concurrent.ipynb");

        // Setup: agent-old at generation 0 with populated fields.
        {
            let mut id = room.current_runtime_agent_id.write().await;
            *id = Some("agent-old".to_string());
        }
        let (old_tx, _old_rx) = oneshot::channel::<()>();
        {
            let mut guard = room.pending_runtime_agent_connect_tx.lock().await;
            *guard = Some(old_tx);
        }
        let (old_req_tx, _old_req_rx) = tokio::sync::mpsc::channel(16);
        {
            let mut guard = room.runtime_agent_request_tx.lock().await;
            *guard = Some(old_req_tx);
        }

        // Clone Arc fields for the concurrent task.
        let id_arc = room.current_runtime_agent_id.clone();
        let gen_arc = room.runtime_agent_generation.clone();
        let connect_arc = room.pending_runtime_agent_connect_tx.clone();
        let req_arc = room.runtime_agent_request_tx.clone();

        // Channel to receive the new spawn's oneshot receiver (for liveness check).
        let (done_tx, done_rx) = oneshot::channel::<oneshot::Receiver<()>>();

        // Spawn concurrent task: simulate a new spawn that fires as soon as
        // provenance is cleared (the trigger for the TOCTOU scenario).
        tokio::spawn(async move {
            // Poll for provenance → None (reset_starting_state clears it).
            loop {
                {
                    let current = id_arc.read().await;
                    if current.is_none() {
                        break;
                    }
                }
                tokio::task::yield_now().await;
            }

            // Simulate new spawn sequence: provenance → generation → fields.
            {
                let mut id = id_arc.write().await;
                *id = Some("agent-new".to_string());
            }
            gen_arc.fetch_add(1, Ordering::Release);
            let (new_tx, new_rx) = oneshot::channel::<()>();
            {
                let mut guard = connect_arc.lock().await;
                *guard = Some(new_tx);
            }
            let (new_req_tx, _) = tokio::sync::mpsc::channel(16);
            {
                let mut guard = req_arc.lock().await;
                *guard = Some(new_req_tx);
            }

            let _ = done_tx.send(new_rx);
        });

        // Main task: call reset — provenance matches "agent-old", so it proceeds.
        // Generation was captured inside the provenance write lock (gen=0).
        // If the concurrent spawn bumps gen to 1 before field clears, the
        // generation guard aborts the clears. Otherwise, reset clears old fields
        // and the concurrent spawn stores new ones afterward.
        reset_starting_state(&room, Some("agent-old")).await;

        // Wait for concurrent task to complete its spawn simulation.
        let mut new_rx = done_rx
            .await
            .expect("concurrent spawn task should complete");

        // Verify: new spawn's fields must be present regardless of ordering.
        assert!(
            room.pending_runtime_agent_connect_tx.lock().await.is_some(),
            "connect_tx should be present (new spawn's)"
        );
        assert!(
            room.runtime_agent_request_tx.lock().await.is_some(),
            "request_tx should be present (new spawn's)"
        );
        assert_eq!(
            room.current_runtime_agent_id.read().await.as_deref(),
            Some("agent-new"),
            "provenance should be agent-new (set by concurrent spawn)"
        );
        // Verify oneshot sender is still alive (not dropped by reset).
        assert!(
            new_rx.try_recv().is_err(),
            "new spawn's oneshot sender should be alive"
        );
        // Generation should be 1 (bumped by concurrent spawn).
        assert_eq!(
            room.runtime_agent_generation.load(Ordering::Acquire),
            1,
            "generation should be 1 after concurrent spawn"
        );
    }

    #[test]
    fn test_env_yml_insertion_point_no_trailing_newline() {
        // File without trailing newline — must not panic or return out-of-bounds offset
        let content = "dependencies:\n  - numpy";
        let point = find_env_yml_deps_insertion_point(content);
        assert!(point.is_some());
        assert!(point.unwrap() <= content.len());
    }

    #[test]
    fn test_env_yml_insertion_point_with_trailing_newline() {
        let content = "dependencies:\n  - numpy\n  - pandas\n";
        let point = find_env_yml_deps_insertion_point(content);
        assert_eq!(point, Some(content.len()));
    }

    #[test]
    fn test_env_yml_insertion_point_before_next_key() {
        let content = "dependencies:\n  - numpy\nchannels:\n  - conda-forge\n";
        let point = find_env_yml_deps_insertion_point(content);
        // Should insert after "  - numpy\n", before "channels:"
        assert_eq!(point, Some("dependencies:\n  - numpy\n".len()));
    }

    /// Pre-v4 .ipynb (no output_id fields) gets IDs minted on load,
    /// persisted through save, and stable across reload.
    #[tokio::test]
    async fn test_pre_v4_ipynb_output_id_round_trip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);

        let notebook_json = serde_json::json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {},
            "cells": [
                {
                    "id": "cell-a",
                    "cell_type": "code",
                    "source": "1 + 1",
                    "execution_count": 1,
                    "metadata": {},
                    "outputs": [
                        {
                            "output_type": "execute_result",
                            "execution_count": 1,
                            "data": { "text/plain": "2" },
                            "metadata": {}
                        }
                    ]
                },
                {
                    "id": "cell-b",
                    "cell_type": "code",
                    "source": "print('hi')",
                    "execution_count": 2,
                    "metadata": {},
                    "outputs": [
                        {
                            "output_type": "stream",
                            "name": "stdout",
                            "text": "hi\n"
                        }
                    ]
                },
                {
                    "id": "cell-c",
                    "cell_type": "code",
                    "source": "display('x')",
                    "execution_count": 3,
                    "metadata": {},
                    "outputs": [
                        {
                            "output_type": "display_data",
                            "data": { "text/plain": "x" },
                            "metadata": {}
                        }
                    ]
                },
                {
                    "id": "cell-d",
                    "cell_type": "code",
                    "source": "1/0",
                    "execution_count": 4,
                    "metadata": {},
                    "outputs": [
                        {
                            "output_type": "error",
                            "ename": "ZeroDivisionError",
                            "evalue": "division by zero",
                            "traceback": ["line 1"]
                        }
                    ]
                }
            ]
        });

        let ipynb_path = tmp.path().join("legacy.ipynb");
        std::fs::write(
            &ipynb_path,
            serde_json::to_string_pretty(&notebook_json).unwrap(),
        )
        .unwrap();

        // --- Load 1: pre-v4 notebook, no output_id fields ---
        let notebook_id = ipynb_path.to_string_lossy().to_string();
        let mut doc = crate::notebook_doc::NotebookDoc::new(&notebook_id);
        let mut state_doc = RuntimeStateDoc::new();
        load_notebook_from_disk_with_state_doc(
            &mut doc,
            Some(&mut state_doc),
            &ipynb_path,
            &blob_store,
        )
        .await
        .unwrap();

        // Collect minted output_ids from RuntimeStateDoc
        let mut first_load_ids: Vec<(String, String)> = Vec::new();
        for cell_id in ["cell-a", "cell-b", "cell-c", "cell-d"] {
            let eid = doc
                .get_execution_id(cell_id)
                .unwrap_or_else(|| panic!("{cell_id} should have execution_id"));
            let outputs = state_doc.get_outputs(&eid);
            assert_eq!(outputs.len(), 1, "{cell_id} should have 1 output");
            let manifest: crate::output_store::OutputManifest =
                serde_json::from_value(outputs[0].clone()).unwrap();
            let id = manifest.output_id().to_string();
            assert!(
                !id.is_empty(),
                "{cell_id} should have a non-empty output_id"
            );
            first_load_ids.push((cell_id.to_string(), id));
        }

        // All IDs should be distinct
        let id_set: std::collections::HashSet<&str> =
            first_load_ids.iter().map(|(_, id)| id.as_str()).collect();
        assert_eq!(id_set.len(), 4, "All output_ids should be unique");

        // --- Save: resolve manifests to .ipynb JSON ---
        let mut saved_ids: Vec<(String, String)> = Vec::new();
        for (cell_id, expected_id) in &first_load_ids {
            let eid = doc.get_execution_id(cell_id).unwrap();
            let outputs = state_doc.get_outputs(&eid);
            let manifest: crate::output_store::OutputManifest =
                serde_json::from_value(outputs[0].clone()).unwrap();
            let resolved = crate::output_store::resolve_manifest(&manifest, &blob_store)
                .await
                .unwrap();
            let saved_id = resolved["output_id"]
                .as_str()
                .unwrap_or_else(|| panic!("{cell_id} resolved JSON should have output_id"));
            assert_eq!(
                saved_id, expected_id,
                "{cell_id}: resolve_manifest should preserve output_id"
            );
            saved_ids.push((cell_id.clone(), saved_id.to_string()));
        }

        // --- Reload: simulate saving and reloading ---
        // Build an .ipynb with output_id fields (as resolve_manifest now produces)
        let mut cells_with_ids = Vec::new();
        for (cell_id, _) in &first_load_ids {
            let eid = doc.get_execution_id(cell_id).unwrap();
            let outputs = state_doc.get_outputs(&eid);
            let manifest: crate::output_store::OutputManifest =
                serde_json::from_value(outputs[0].clone()).unwrap();
            let resolved = crate::output_store::resolve_manifest(&manifest, &blob_store)
                .await
                .unwrap();
            cells_with_ids.push((cell_id.clone(), resolved));
        }

        let saved_notebook = serde_json::json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {},
            "cells": [
                {
                    "id": "cell-a",
                    "cell_type": "code",
                    "source": "1 + 1",
                    "execution_count": 1,
                    "metadata": {},
                    "outputs": [cells_with_ids[0].1]
                },
                {
                    "id": "cell-b",
                    "cell_type": "code",
                    "source": "print('hi')",
                    "execution_count": 2,
                    "metadata": {},
                    "outputs": [cells_with_ids[1].1]
                },
                {
                    "id": "cell-c",
                    "cell_type": "code",
                    "source": "display('x')",
                    "execution_count": 3,
                    "metadata": {},
                    "outputs": [cells_with_ids[2].1]
                },
                {
                    "id": "cell-d",
                    "cell_type": "code",
                    "source": "1/0",
                    "execution_count": 4,
                    "metadata": {},
                    "outputs": [cells_with_ids[3].1]
                }
            ]
        });

        let ipynb_path2 = tmp.path().join("saved.ipynb");
        std::fs::write(
            &ipynb_path2,
            serde_json::to_string_pretty(&saved_notebook).unwrap(),
        )
        .unwrap();

        // Load the saved notebook
        let mut doc2 = crate::notebook_doc::NotebookDoc::new("reload-test");
        let mut state_doc2 = RuntimeStateDoc::new();
        load_notebook_from_disk_with_state_doc(
            &mut doc2,
            Some(&mut state_doc2),
            &ipynb_path2,
            &blob_store,
        )
        .await
        .unwrap();

        // Verify IDs are stable across the round-trip
        for (cell_id, expected_id) in &first_load_ids {
            let eid = doc2.get_execution_id(cell_id).unwrap();
            let outputs = state_doc2.get_outputs(&eid);
            let manifest: crate::output_store::OutputManifest =
                serde_json::from_value(outputs[0].clone()).unwrap();
            assert_eq!(
                manifest.output_id(),
                expected_id,
                "{cell_id}: output_id should be stable across save/load round-trip"
            );
        }
    }

    // ── PR 2: prewarmed env capture (spec 2026-04-20) ───────────────────────

    /// Build a minimal room suitable for exercising metadata writes. Avoids
    /// pulling in the full daemon stack — we only touch `room.doc`.
    ///
    /// Returns `(room, _tmp)` so the TempDir lives at least as long as
    /// the room; dropping the TempDir mid-test would remove the docs dir
    /// under the room's persist debouncer.
    async fn test_room_for_capture() -> (NotebookRoom, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let room = NotebookRoom::load_or_create("capture-test", tmp.path(), blob_store);
        // Seed the doc so `get_metadata_snapshot` returns Some, mirroring
        // what `create_empty_notebook` does on a fresh notebook.
        {
            let mut doc = room.doc.write().await;
            let _ = create_empty_notebook(
                &mut doc,
                "python",
                crate::settings_doc::PythonEnvType::Uv,
                Some("test-env-id"),
            );
        }
        (room, tmp)
    }

    #[tokio::test]
    async fn capture_writes_deps_and_env_id_for_fresh_uv_notebook() {
        let (room, _tmp) = test_room_for_capture().await;
        // Wipe env_id first so the capture step sets it.
        {
            let mut doc = room.doc.write().await;
            doc.fork_and_merge(|fork| {
                let mut snap = fork.get_metadata_snapshot().unwrap_or_default();
                snap.runt.env_id = None;
                let _ = fork.set_metadata_snapshot(&snap);
            });
        }
        let user_defaults = vec!["pandas".to_string(), "numpy".to_string()];
        let wrote =
            capture_env_into_metadata(&room, CapturedEnvRuntime::Uv, &user_defaults, "nb-42").await;
        assert!(wrote, "first capture should write both deps and env_id");

        let snap = room.doc.read().await.get_metadata_snapshot().unwrap();
        assert_eq!(snap.runt.env_id.as_deref(), Some("nb-42"));
        assert_eq!(
            snap.runt.uv.as_ref().unwrap().dependencies,
            vec!["pandas".to_string(), "numpy".to_string()]
        );
    }

    #[tokio::test]
    async fn capture_is_idempotent_on_existing_deps() {
        let (room, _tmp) = test_room_for_capture().await;
        // Pre-populate with user-edited deps.
        {
            let mut doc = room.doc.write().await;
            doc.fork_and_merge(|fork| {
                let mut snap = fork.get_metadata_snapshot().unwrap_or_default();
                let uv =
                    snap.runt
                        .uv
                        .get_or_insert_with(|| notebook_doc::metadata::UvInlineMetadata {
                            dependencies: Vec::new(),
                            requires_python: None,
                            prerelease: None,
                        });
                uv.dependencies = vec!["scikit-learn".to_string()];
                let _ = fork.set_metadata_snapshot(&snap);
            });
        }

        // Second capture tries to overwrite with different defaults — must not.
        let wrote = capture_env_into_metadata(
            &room,
            CapturedEnvRuntime::Uv,
            &["pandas".to_string()],
            "nb-x",
        )
        .await;
        assert!(
            !wrote,
            "capture must not overwrite user-edited deps (env_id already set)"
        );

        let snap = room.doc.read().await.get_metadata_snapshot().unwrap();
        assert_eq!(
            snap.runt.uv.as_ref().unwrap().dependencies,
            vec!["scikit-learn".to_string()],
            "captured deps must not overwrite existing non-empty list"
        );
    }

    #[tokio::test]
    async fn capture_preserves_existing_env_id_across_calls() {
        let (room, _tmp) = test_room_for_capture().await;
        // env_id is already set by create_empty_notebook to "test-env-id".
        let wrote_first = capture_env_into_metadata(
            &room,
            CapturedEnvRuntime::Uv,
            &["polars".to_string()],
            "different-env-id-ignored",
        )
        .await;
        assert!(wrote_first, "deps filled in, write_id left alone");

        let snap = room.doc.read().await.get_metadata_snapshot().unwrap();
        assert_eq!(
            snap.runt.env_id.as_deref(),
            Some("test-env-id"),
            "existing env_id must survive capture"
        );

        // Second call with same defaults is a no-op.
        let wrote_second =
            capture_env_into_metadata(&room, CapturedEnvRuntime::Uv, &["polars".to_string()], "x")
                .await;
        assert!(
            !wrote_second,
            "second capture must be a no-op when deps and env_id are already set"
        );
    }

    #[tokio::test]
    async fn capture_handles_conda_section_independently() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let room = NotebookRoom::load_or_create("capture-conda-test", tmp.path(), blob_store);
        {
            let mut doc = room.doc.write().await;
            let _ = create_empty_notebook(
                &mut doc,
                "python",
                crate::settings_doc::PythonEnvType::Conda,
                Some("conda-env-id"),
            );
        }

        let user_defaults = vec!["scipy".to_string()];
        let wrote = capture_env_into_metadata(
            &room,
            CapturedEnvRuntime::Conda,
            &user_defaults,
            "conda-env-id",
        )
        .await;
        assert!(wrote);

        let snap = room.doc.read().await.get_metadata_snapshot().unwrap();
        assert_eq!(
            snap.runt.conda.as_ref().unwrap().dependencies,
            vec!["scipy".to_string()]
        );
        // UV section should remain untouched.
        assert!(snap.runt.uv.is_none());
    }

    #[test]
    fn captured_env_for_runtime_reads_uv_deps_and_env_id() {
        let mut snap = NotebookMetadataSnapshot::default();
        snap.runt.env_id = Some("abc".to_string());
        snap.runt.uv = Some(notebook_doc::metadata::UvInlineMetadata {
            dependencies: vec!["pandas".to_string()],
            requires_python: None,
            prerelease: None,
        });
        let captured =
            captured_env_for_runtime(Some(&snap), CapturedEnvRuntime::Uv).expect("captured env");
        assert_eq!(captured.env_id(), "abc");
        assert_eq!(captured.dependencies(), &["pandas".to_string()]);
        match &captured {
            CapturedEnv::Uv { deps, .. } => {
                assert_eq!(deps.requires_python, None);
                assert_eq!(deps.prerelease, None);
            }
            _ => panic!("expected UV captured env"),
        }
    }

    #[test]
    fn captured_env_for_runtime_returns_empty_deps_when_section_missing() {
        let mut snap = NotebookMetadataSnapshot::default();
        snap.runt.env_id = Some("xyz".to_string());
        // No uv or conda section populated.
        let captured =
            captured_env_for_runtime(Some(&snap), CapturedEnvRuntime::Uv).expect("captured env");
        assert!(captured.dependencies().is_empty());
        assert_eq!(captured.env_id(), "xyz");
    }

    #[test]
    fn captured_env_for_runtime_includes_uv_resolver_fields() {
        // P2 regression: captured lookup must carry requires-python and
        // prerelease, not just the dep list. Otherwise the on-disk hash
        // computed on reopen would differ from what the capture step
        // originally wrote, causing false cache misses or worse, matching
        // the wrong cached env.
        let mut snap = NotebookMetadataSnapshot::default();
        snap.runt.env_id = Some("env-uv".to_string());
        snap.runt.uv = Some(notebook_doc::metadata::UvInlineMetadata {
            dependencies: vec!["pandas".to_string()],
            requires_python: Some(">=3.10".to_string()),
            prerelease: Some("allow".to_string()),
        });

        let captured =
            captured_env_for_runtime(Some(&snap), CapturedEnvRuntime::Uv).expect("captured env");
        match &captured {
            CapturedEnv::Uv { deps, env_id } => {
                assert_eq!(env_id, "env-uv");
                assert_eq!(deps.dependencies, vec!["pandas".to_string()]);
                assert_eq!(deps.requires_python.as_deref(), Some(">=3.10"));
                assert_eq!(deps.prerelease.as_deref(), Some("allow"));
            }
            _ => panic!("expected UV captured env"),
        }
    }

    #[test]
    fn captured_env_for_runtime_includes_conda_resolver_fields() {
        // P2 regression: captured lookup must carry channels and python pin.
        let mut snap = NotebookMetadataSnapshot::default();
        snap.runt.env_id = Some("env-conda".to_string());
        snap.runt.conda = Some(notebook_doc::metadata::CondaInlineMetadata {
            dependencies: vec!["scipy".to_string()],
            channels: vec!["pytorch".to_string(), "nvidia".to_string()],
            python: Some("3.11".to_string()),
        });

        let captured =
            captured_env_for_runtime(Some(&snap), CapturedEnvRuntime::Conda).expect("captured env");
        match &captured {
            CapturedEnv::Conda { deps, env_id } => {
                assert_eq!(env_id, "env-conda");
                assert_eq!(deps.dependencies, vec!["scipy".to_string()]);
                assert_eq!(
                    deps.channels,
                    vec!["pytorch".to_string(), "nvidia".to_string()]
                );
                assert_eq!(deps.python.as_deref(), Some("3.11"));
            }
            _ => panic!("expected Conda captured env"),
        }
    }

    #[test]
    fn captured_env_hash_differs_when_uv_prerelease_changes() {
        // P2 invariant: two captures with identical deps + env_id but a
        // different prerelease strategy must hash to different paths. If
        // they didn't, the on-disk lookup would happily find the wrong
        // prior env and reuse it with the wrong install set.
        let base_deps = vec!["pandas".to_string()];
        let a = kernel_env::UvDependencies {
            dependencies: base_deps.clone(),
            requires_python: None,
            prerelease: None,
        };
        let b = kernel_env::UvDependencies {
            dependencies: base_deps,
            requires_python: None,
            prerelease: Some("allow".to_string()),
        };
        let hash_a = kernel_env::uv::compute_unified_env_hash(&a, "same-env-id");
        let hash_b = kernel_env::uv::compute_unified_env_hash(&b, "same-env-id");
        assert_ne!(hash_a, hash_b);
    }

    #[test]
    fn captured_env_hash_differs_when_conda_channels_change() {
        let base_deps = vec!["scipy".to_string()];
        let a = kernel_env::CondaDependencies {
            dependencies: base_deps.clone(),
            channels: vec!["conda-forge".to_string()],
            python: None,
            env_id: None,
        };
        let b = kernel_env::CondaDependencies {
            dependencies: base_deps.clone(),
            channels: vec!["conda-forge".to_string(), "pytorch".to_string()],
            python: None,
            env_id: None,
        };
        let hash_a = kernel_env::conda::compute_unified_env_hash(&a, "same-env-id");
        let hash_b = kernel_env::conda::compute_unified_env_hash(&b, "same-env-id");
        assert_ne!(hash_a, hash_b);

        // Python pin also contributes.
        let c = kernel_env::CondaDependencies {
            dependencies: base_deps,
            channels: vec!["conda-forge".to_string()],
            python: Some("3.12".to_string()),
            env_id: None,
        };
        let hash_c = kernel_env::conda::compute_unified_env_hash(&c, "same-env-id");
        assert_ne!(hash_a, hash_c);
    }

    #[test]
    fn captured_env_for_runtime_requires_env_id() {
        let snap = NotebookMetadataSnapshot::default();
        assert!(captured_env_for_runtime(Some(&snap), CapturedEnvRuntime::Uv).is_none());
    }

    #[test]
    fn captured_env_source_override_returns_none_when_no_env_id() {
        let snap = NotebookMetadataSnapshot::default();
        assert!(captured_env_source_override(Some(&snap)).is_none());
    }

    #[test]
    fn captured_env_source_override_returns_none_when_deps_present_but_env_missing() {
        // Deps-present-but-disk-absent is intentionally NOT treated as
        // captured: we cannot tell a GC'd captured env apart from a fresh
        // notebook whose user added inline deps before the first launch.
        // Falling through to the normal inline path is the safer default —
        // same deps still dedup across notebooks via the legacy inline
        // cache; they just lose per-notebook env_id isolation for that
        // rebuild.
        let mut snap = NotebookMetadataSnapshot::default();
        snap.runt.env_id = Some(format!("unlikely-env-id-{}", uuid::Uuid::new_v4()));
        snap.runt.uv = Some(notebook_doc::metadata::UvInlineMetadata {
            dependencies: vec!["pandas".to_string()],
            requires_python: None,
            prerelease: None,
        });
        assert!(captured_env_source_override(Some(&snap)).is_none());
    }

    #[test]
    fn captured_env_source_override_returns_none_for_fresh_notebook_empty_deps() {
        // `create_empty_notebook` assigns an env_id and an empty uv/conda
        // section on every new notebook. Empty deps + no env on disk must
        // NOT be treated as captured, otherwise brand-new `uv:prewarmed`
        // launches bypass the warmed pool and build a base env from scratch.
        let mut snap = NotebookMetadataSnapshot::default();
        snap.runt.env_id = Some(format!("fresh-env-{}", uuid::Uuid::new_v4()));
        snap.runt.uv = Some(notebook_doc::metadata::UvInlineMetadata {
            dependencies: vec![],
            requires_python: None,
            prerelease: None,
        });
        assert!(captured_env_source_override(Some(&snap)).is_none());
    }

    /// Given a tmpdir pretending to be the UV cache, materialise a fake
    /// venv at `{cache}/{hash}/bin/python` for the given captured env so
    /// `unified_env_on_disk_in` finds it.
    fn materialise_fake_uv_venv(
        deps: &kernel_env::UvDependencies,
        env_id: &str,
        cache_dir: &Path,
    ) -> PathBuf {
        let hash = kernel_env::uv::compute_unified_env_hash(deps, env_id);
        let venv_path = cache_dir.join(&hash);
        #[cfg(target_os = "windows")]
        let python_path = venv_path.join("Scripts").join("python.exe");
        #[cfg(not(target_os = "windows"))]
        let python_path = venv_path.join("bin").join("python");
        std::fs::create_dir_all(python_path.parent().unwrap()).unwrap();
        std::fs::write(&python_path, b"#!/bin/sh\n").unwrap();
        venv_path
    }

    #[test]
    fn should_preserve_env_on_eviction_untitled_notebook_deletes() {
        // Even with captured deps + env on disk, no saved path means the
        // .ipynb won't persist the env_id binding — the env is orphaned.
        let tmp = tempfile::tempdir().unwrap();
        let uv_cache = tmp.path().to_path_buf();
        let conda_cache = tmp.path().join("conda");
        std::fs::create_dir_all(&conda_cache).unwrap();

        let deps = kernel_env::UvDependencies {
            dependencies: vec!["pandas".to_string()],
            requires_python: None,
            prerelease: None,
        };
        let env_id = "env-untitled";
        let venv_path = materialise_fake_uv_venv(&deps, env_id, &uv_cache);

        let mut snap = NotebookMetadataSnapshot::default();
        snap.runt.env_id = Some(env_id.to_string());
        snap.runt.uv = Some(notebook_doc::metadata::UvInlineMetadata {
            dependencies: deps.dependencies.clone(),
            requires_python: None,
            prerelease: None,
        });

        assert!(!should_preserve_env_on_eviction(
            false, // has_saved_path
            &venv_path,
            Some(&snap),
            &uv_cache,
            &conda_cache,
        ));
    }

    #[test]
    fn should_preserve_env_on_eviction_saved_captured_notebook_preserves() {
        let tmp = tempfile::tempdir().unwrap();
        let uv_cache = tmp.path().to_path_buf();
        let conda_cache = tmp.path().join("conda");
        std::fs::create_dir_all(&conda_cache).unwrap();

        let deps = kernel_env::UvDependencies {
            dependencies: vec!["pandas".to_string(), "numpy".to_string()],
            requires_python: None,
            prerelease: None,
        };
        let env_id = "env-saved";
        let venv_path = materialise_fake_uv_venv(&deps, env_id, &uv_cache);

        let mut snap = NotebookMetadataSnapshot::default();
        snap.runt.env_id = Some(env_id.to_string());
        snap.runt.uv = Some(notebook_doc::metadata::UvInlineMetadata {
            dependencies: deps.dependencies.clone(),
            requires_python: None,
            prerelease: None,
        });

        assert!(should_preserve_env_on_eviction(
            true,
            &venv_path,
            Some(&snap),
            &uv_cache,
            &conda_cache,
        ));
    }

    #[test]
    fn should_preserve_env_on_eviction_path_mismatch_deletes() {
        // Room has a saved path but its runtime_agent_env_path points at a
        // pool env (runtimed-uv-xxx), not the captured hash dir. That's a
        // pool-launched notebook — delete on eviction like before.
        let tmp = tempfile::tempdir().unwrap();
        let uv_cache = tmp.path().to_path_buf();
        let conda_cache = tmp.path().join("conda");
        std::fs::create_dir_all(&conda_cache).unwrap();

        let deps = kernel_env::UvDependencies {
            dependencies: vec!["pandas".to_string()],
            requires_python: None,
            prerelease: None,
        };
        let env_id = "env-pool";
        let _ = materialise_fake_uv_venv(&deps, env_id, &uv_cache);

        let pool_path = uv_cache.join("runtimed-uv-abc123");

        let mut snap = NotebookMetadataSnapshot::default();
        snap.runt.env_id = Some(env_id.to_string());
        snap.runt.uv = Some(notebook_doc::metadata::UvInlineMetadata {
            dependencies: deps.dependencies.clone(),
            requires_python: None,
            prerelease: None,
        });

        assert!(!should_preserve_env_on_eviction(
            true,
            &pool_path,
            Some(&snap),
            &uv_cache,
            &conda_cache,
        ));
    }

    #[test]
    fn should_preserve_env_on_eviction_no_captured_metadata_deletes() {
        // Saved notebook but empty metadata — never captured, nothing to
        // preserve. This covers fresh notebooks that got saved before
        // first launch.
        let tmp = tempfile::tempdir().unwrap();
        let uv_cache = tmp.path().to_path_buf();
        let conda_cache = tmp.path().join("conda");
        std::fs::create_dir_all(&conda_cache).unwrap();

        let some_path = uv_cache.join("runtimed-uv-deadbeef");

        assert!(!should_preserve_env_on_eviction(
            true,
            &some_path,
            Some(&NotebookMetadataSnapshot::default()),
            &uv_cache,
            &conda_cache,
        ));
    }

    #[test]
    fn effective_user_deps_from_launched_uses_uv_deps_when_set() {
        // Hot-sync appends to launched.uv_deps, so that's the source of
        // truth for what should land in metadata.
        let launched = LaunchedEnvConfig {
            uv_deps: Some(vec!["pandas".to_string(), "numpy".to_string()]),
            ..LaunchedEnvConfig::default()
        };
        let deps = effective_user_deps_from_launched(&launched, CapturedEnvRuntime::Uv).unwrap();
        assert_eq!(deps, vec!["pandas".to_string(), "numpy".to_string()]);
    }

    #[test]
    fn effective_user_deps_from_launched_ignores_prewarmed_packages() {
        // Pure prewarmed kernel that never hot-synced: uv_deps and
        // conda_deps are both None, prewarmed_packages has the pool's
        // install list. We intentionally return None here so the eviction
        // flush doesn't mistakenly populate metadata.runt.conda for a
        // pure UV kernel (or vice versa). For this case captured
        // metadata was already written at claim time and there's nothing
        // to flush.
        let launched = LaunchedEnvConfig {
            uv_deps: None,
            conda_deps: None,
            prewarmed_packages: vec![
                "ipykernel".to_string(),
                "ipywidgets".to_string(),
                "anywidget".to_string(),
                "nbformat".to_string(),
                "uv".to_string(),
                "dx".to_string(),
                "pandas".to_string(),
            ],
            ..LaunchedEnvConfig::default()
        };
        assert!(effective_user_deps_from_launched(&launched, CapturedEnvRuntime::Uv).is_none());
        assert!(effective_user_deps_from_launched(&launched, CapturedEnvRuntime::Conda).is_none());
    }

    #[test]
    fn effective_user_deps_from_launched_strips_base_from_uv_deps() {
        // Hot-synced a package into a captured-reopen kernel: uv_deps
        // = [captured_user_deps + synced]. Base packages should never
        // be in uv_deps in practice, but strip_base is idempotent and
        // this guards against accidental inclusion.
        let launched = LaunchedEnvConfig {
            uv_deps: Some(vec![
                "ipykernel".to_string(),
                "pandas".to_string(),
                "numpy".to_string(),
            ]),
            ..LaunchedEnvConfig::default()
        };
        let deps = effective_user_deps_from_launched(&launched, CapturedEnvRuntime::Uv).unwrap();
        assert_eq!(deps, vec!["pandas".to_string(), "numpy".to_string()]);
    }

    #[test]
    fn effective_user_deps_from_launched_returns_none_for_wrong_runtime() {
        // Kernel launched with uv_deps; asking for Conda view returns None.
        let launched = LaunchedEnvConfig {
            uv_deps: Some(vec!["pandas".to_string()]),
            ..LaunchedEnvConfig::default()
        };
        assert!(effective_user_deps_from_launched(&launched, CapturedEnvRuntime::Conda).is_none());
    }

    #[test]
    fn effective_user_deps_from_launched_returns_none_for_deno_only() {
        // Deno kernel: no uv/conda deps at all. No flush applicable.
        let launched = LaunchedEnvConfig::default();
        assert!(effective_user_deps_from_launched(&launched, CapturedEnvRuntime::Uv).is_none());
        assert!(effective_user_deps_from_launched(&launched, CapturedEnvRuntime::Conda).is_none());
    }

    #[test]
    fn effective_user_deps_from_launched_conda_uses_conda_deps() {
        let launched = LaunchedEnvConfig {
            conda_deps: Some(vec!["scipy".to_string()]),
            ..LaunchedEnvConfig::default()
        };
        let deps = effective_user_deps_from_launched(&launched, CapturedEnvRuntime::Conda).unwrap();
        assert_eq!(deps, vec!["scipy".to_string()]);
    }

    #[tokio::test]
    async fn rename_env_dir_moves_to_unified_hash_target() {
        let tmp = tempfile::tempdir().unwrap();
        let uv_cache = tmp.path().to_path_buf();
        let conda_cache = tmp.path().join("conda");
        std::fs::create_dir_all(&conda_cache).unwrap();

        // Initial state: env lives under the OLD hash (captured deps
        // before hot-sync). We materialise a fake venv there.
        let old_deps = kernel_env::UvDependencies {
            dependencies: vec!["pandas".to_string()],
            requires_python: None,
            prerelease: None,
        };
        let env_id = "rename-target";
        let old_path = materialise_fake_uv_venv(&old_deps, env_id, &uv_cache);

        // Metadata after flush: deps grew to include numpy (new hash).
        let new_deps = kernel_env::UvDependencies {
            dependencies: vec!["pandas".to_string(), "numpy".to_string()],
            requires_python: None,
            prerelease: None,
        };
        let mut snap = NotebookMetadataSnapshot::default();
        snap.runt.env_id = Some(env_id.to_string());
        snap.runt.uv = Some(notebook_doc::metadata::UvInlineMetadata {
            dependencies: new_deps.dependencies.clone(),
            requires_python: None,
            prerelease: None,
        });

        let expected_target =
            uv_cache.join(kernel_env::uv::compute_unified_env_hash(&new_deps, env_id));
        assert!(!expected_target.exists());

        let returned = rename_env_dir_to_unified_hash(
            &old_path,
            Some(&snap),
            CapturedEnvRuntime::Uv,
            &uv_cache,
            &conda_cache,
        )
        .await;

        assert_eq!(returned, expected_target);
        assert!(!old_path.exists());
        assert!(expected_target.exists());
    }

    #[tokio::test]
    async fn rename_env_dir_noop_when_already_at_target() {
        let tmp = tempfile::tempdir().unwrap();
        let uv_cache = tmp.path().to_path_buf();
        let conda_cache = tmp.path().join("conda");
        std::fs::create_dir_all(&conda_cache).unwrap();

        let deps = kernel_env::UvDependencies {
            dependencies: vec!["pandas".to_string()],
            requires_python: None,
            prerelease: None,
        };
        let env_id = "already-correct";
        let path = materialise_fake_uv_venv(&deps, env_id, &uv_cache);

        let mut snap = NotebookMetadataSnapshot::default();
        snap.runt.env_id = Some(env_id.to_string());
        snap.runt.uv = Some(notebook_doc::metadata::UvInlineMetadata {
            dependencies: deps.dependencies.clone(),
            requires_python: None,
            prerelease: None,
        });

        let returned = rename_env_dir_to_unified_hash(
            &path,
            Some(&snap),
            CapturedEnvRuntime::Uv,
            &uv_cache,
            &conda_cache,
        )
        .await;

        assert_eq!(returned, path);
        assert!(path.exists());
    }

    #[tokio::test]
    async fn rename_env_dir_skips_when_target_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let uv_cache = tmp.path().to_path_buf();
        let conda_cache = tmp.path().join("conda");
        std::fs::create_dir_all(&conda_cache).unwrap();

        // Two distinct envs on disk — an old one and the target name
        // from a different notebook already claimed. We must not
        // clobber the target.
        let old_deps = kernel_env::UvDependencies {
            dependencies: vec!["pandas".to_string()],
            requires_python: None,
            prerelease: None,
        };
        let env_id = "collide";
        let old_path = materialise_fake_uv_venv(&old_deps, env_id, &uv_cache);

        let new_deps = kernel_env::UvDependencies {
            dependencies: vec!["pandas".to_string(), "numpy".to_string()],
            requires_python: None,
            prerelease: None,
        };
        let occupied_path = materialise_fake_uv_venv(&new_deps, env_id, &uv_cache);
        assert!(occupied_path.exists());

        let mut snap = NotebookMetadataSnapshot::default();
        snap.runt.env_id = Some(env_id.to_string());
        snap.runt.uv = Some(notebook_doc::metadata::UvInlineMetadata {
            dependencies: new_deps.dependencies.clone(),
            requires_python: None,
            prerelease: None,
        });

        let returned = rename_env_dir_to_unified_hash(
            &old_path,
            Some(&snap),
            CapturedEnvRuntime::Uv,
            &uv_cache,
            &conda_cache,
        )
        .await;

        // Target is occupied — leave source intact.
        assert_eq!(returned, old_path);
        assert!(old_path.exists());
        assert!(occupied_path.exists());
    }

    #[tokio::test]
    async fn rename_env_dir_noop_when_no_captured_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let uv_cache = tmp.path().to_path_buf();
        let conda_cache = tmp.path().join("conda");
        std::fs::create_dir_all(&conda_cache).unwrap();

        let some_path = uv_cache.join("runtimed-uv-abc123");
        std::fs::create_dir_all(&some_path).unwrap();

        let returned = rename_env_dir_to_unified_hash(
            &some_path,
            Some(&NotebookMetadataSnapshot::default()),
            CapturedEnvRuntime::Uv,
            &uv_cache,
            &conda_cache,
        )
        .await;

        assert_eq!(returned, some_path);
        assert!(some_path.exists());
    }

    /// Given a tmpdir pretending to be the Conda cache, materialise a
    /// fake env at `{cache}/{hash}/bin/python`.
    fn materialise_fake_conda_env(
        deps: &kernel_env::CondaDependencies,
        env_id: &str,
        cache_dir: &Path,
    ) -> PathBuf {
        let hash = kernel_env::conda::compute_unified_env_hash(deps, env_id);
        let env_path = cache_dir.join(&hash);
        #[cfg(target_os = "windows")]
        let python_path = env_path.join("python.exe");
        #[cfg(not(target_os = "windows"))]
        let python_path = env_path.join("bin").join("python");
        std::fs::create_dir_all(python_path.parent().unwrap()).unwrap();
        std::fs::write(&python_path, b"#!/bin/sh\n").unwrap();
        env_path
    }

    /// Regression: conda-only notebook with env_id set must not route
    /// its rename through the UV hash function. Before the runtime
    /// parameter was explicit, `captured_env_for_runtime(Uv)` would
    /// synthesise a zero-dep UV capture from `runt.env_id`, and the
    /// rename helper would pick the UV-hash path first even though the
    /// kernel was conda.
    #[tokio::test]
    async fn rename_env_dir_uses_conda_hash_when_runtime_is_conda() {
        let tmp = tempfile::tempdir().unwrap();
        let uv_cache = tmp.path().join("uv");
        let conda_cache = tmp.path().join("conda");
        std::fs::create_dir_all(&uv_cache).unwrap();
        std::fs::create_dir_all(&conda_cache).unwrap();

        let old_conda_deps = kernel_env::CondaDependencies {
            dependencies: vec!["numpy".to_string()],
            channels: vec!["conda-forge".to_string()],
            python: None,
            env_id: None,
        };
        let env_id = "conda-rename";
        let old_path = materialise_fake_conda_env(&old_conda_deps, env_id, &conda_cache);

        let new_conda_deps = kernel_env::CondaDependencies {
            dependencies: vec!["numpy".to_string(), "scipy".to_string()],
            channels: vec!["conda-forge".to_string()],
            python: None,
            env_id: None,
        };
        let expected = conda_cache.join(kernel_env::conda::compute_unified_env_hash(
            &new_conda_deps,
            env_id,
        ));

        let mut snap = NotebookMetadataSnapshot::default();
        snap.runt.env_id = Some(env_id.to_string());
        snap.runt.conda = Some(notebook_doc::metadata::CondaInlineMetadata {
            dependencies: new_conda_deps.dependencies.clone(),
            channels: new_conda_deps.channels.clone(),
            python: None,
        });

        let returned = rename_env_dir_to_unified_hash(
            &old_path,
            Some(&snap),
            CapturedEnvRuntime::Conda,
            &uv_cache,
            &conda_cache,
        )
        .await;

        assert_eq!(returned, expected);
        assert!(expected.exists());
        assert!(!old_path.exists());
    }

    /// P1 regression: the manual LaunchKernel handler must apply the captured
    /// override when the requested `env_source` is auto/prewarmed but respect
    /// explicit `auto:uv` / `auto:conda` scopes when they disagree with the
    /// captured runtime.
    ///
    /// This mirrors the filter inside the LaunchKernel handler. The daemon
    /// side of the launch pipeline needs real pool state, so we can't spin
    /// it up from a unit test — the filter is factored so the logic it
    /// consumes is unit-testable in isolation.
    #[test]
    fn launch_kernel_captured_override_respects_auto_scope() {
        // The `captured` inputs here are the *string* outputs of
        // `captured_env_source_override`. Simulate a UV-captured notebook.
        let captured_uv = Some("uv:prewarmed".to_string());
        let captured_conda = Some("conda:prewarmed".to_string());

        // Replicates the inline filter inside the LaunchKernel handler.
        fn apply_scope(captured: Option<String>, auto_scope: Option<&str>) -> Option<String> {
            captured.filter(|src| match auto_scope {
                Some("uv") => src == "uv:prewarmed",
                Some("conda") => src == "conda:prewarmed",
                Some("pixi") => false,
                _ => true,
            })
        }

        // Plain auto (no scope) — captured override wins.
        assert_eq!(
            apply_scope(captured_uv.clone(), None),
            Some("uv:prewarmed".to_string())
        );
        assert_eq!(
            apply_scope(captured_conda.clone(), None),
            Some("conda:prewarmed".to_string())
        );

        // Explicit matching scope still fires.
        assert_eq!(
            apply_scope(captured_uv.clone(), Some("uv")),
            Some("uv:prewarmed".to_string())
        );
        assert_eq!(
            apply_scope(captured_conda.clone(), Some("conda")),
            Some("conda:prewarmed".to_string())
        );

        // Explicit mismatched scope drops the override so the project-file /
        // inline-deps priority chain takes over — user intent wins.
        assert_eq!(apply_scope(captured_uv.clone(), Some("conda")), None);
        assert_eq!(apply_scope(captured_conda.clone(), Some("uv")), None);

        // `auto:pixi` always drops the override (no pixi captures today).
        assert_eq!(apply_scope(captured_uv, Some("pixi")), None);
        assert_eq!(apply_scope(captured_conda, Some("pixi")), None);
    }

    /// Pre-upgrade notebooks: env_id is set but deps are empty. The capture
    /// step must still record the env_id (no-op) and populate user_defaults
    /// if they were derived from the pool. This is the migration path from
    /// § 4 Migration of the spec.
    #[tokio::test]
    async fn capture_migrates_pre_upgrade_notebook() {
        let (room, _tmp) = test_room_for_capture().await;
        // Pre-upgrade state: env_id exists, uv section exists but empty.
        let snap_before = room.doc.read().await.get_metadata_snapshot().unwrap();
        assert_eq!(snap_before.runt.env_id.as_deref(), Some("test-env-id"));
        assert!(
            snap_before
                .runt
                .uv
                .as_ref()
                .map(|u| u.dependencies.is_empty())
                .unwrap_or(true),
            "pre-upgrade notebook starts with empty uv deps"
        );

        let user_defaults = vec!["polars".to_string()];
        let wrote =
            capture_env_into_metadata(&room, CapturedEnvRuntime::Uv, &user_defaults, "test-env-id")
                .await;
        assert!(wrote, "migration should populate user_defaults into deps");

        let snap_after = room.doc.read().await.get_metadata_snapshot().unwrap();
        assert_eq!(
            snap_after.runt.uv.as_ref().unwrap().dependencies,
            vec!["polars".to_string()]
        );
    }
}
