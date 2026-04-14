//! KernelConnection trait — internal IO boundary for the runtime agent.
//!
//! This trait separates ZeroMQ IO concerns (sending execute requests, reading
//! completions, signalling interrupts) from queue/state management in the
//! runtime agent's select loop. It is NOT a plugin interface — the only
//! implementation is `JupyterKernel` in `jupyter_kernel.rs`.
//!
//! By programming the runtime agent against this trait, we can:
//! - Own the kernel directly
//! - Test queue logic with a mock kernel
//! - Clearly delineate what crosses the ZeroMQ boundary

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use notebook_doc::presence::PresenceState;
use notebook_doc::runtime_state::RuntimeStateDoc;
use tokio::sync::{broadcast, mpsc, RwLock};

use crate::blob_store::BlobStore;
use crate::kernel_manager::QueueCommand;
use crate::protocol::{CompletionItem, HistoryEntry, NotebookBroadcast};
use crate::PooledEnv;
use notebook_protocol::protocol::LaunchedEnvConfig;

/// Configuration for launching a kernel.
///
/// Bundles the parameters that `KernelConnection::launch` needs beyond the
/// shared references. Extracted so callers don't have to pass 6+ positional
/// arguments.
pub struct KernelLaunchConfig {
    /// Kernel type identifier (e.g., "python", "deno").
    pub kernel_type: String,
    /// Environment source label (e.g., "uv:inline", "conda:prewarmed").
    pub env_source: String,
    /// Path to the notebook file, if saved.
    pub notebook_path: Option<PathBuf>,
    /// Environment configuration snapshot at launch time.
    pub launched_config: LaunchedEnvConfig,
    /// Extra environment variables to set in the kernel process.
    pub env_vars: Vec<(String, String)>,
    /// Prewarmed pool environment, if one was claimed.
    pub pooled_env: Option<PooledEnv>,
}

/// Shared references that the kernel needs but does not own.
///
/// These are `Arc`/`broadcast` handles held by the runtime agent and passed
/// into the kernel at launch time. Grouped here so `launch()` takes two
/// structs instead of a dozen parameters.
pub struct KernelSharedRefs {
    /// Per-notebook runtime state document (daemon-authoritative).
    pub state_doc: Arc<RwLock<RuntimeStateDoc>>,
    /// Notification channel for RuntimeStateDoc changes.
    pub state_changed_tx: broadcast::Sender<()>,
    /// Content-addressed blob store for output manifests.
    pub blob_store: Arc<BlobStore>,
    /// Broadcast channel for notebook events to connected peers.
    pub broadcast_tx: broadcast::Sender<NotebookBroadcast>,
    /// Transient peer state (cursors, selections, kernel status).
    pub presence: Arc<RwLock<PresenceState>>,
    /// Broadcast channel for presence frames.
    pub presence_tx: broadcast::Sender<(String, Vec<u8>)>,
}

/// Internal abstraction over the Jupyter ZeroMQ IO boundary.
///
/// The runtime agent's select loop owns a value of this type directly (no
/// Arc/Mutex wrapper). All methods that touch ZeroMQ channels live here;
/// queue state and CRDT writes live in the agent.
///
/// # Async trait
///
/// Methods are async because they send/receive on ZeroMQ sockets. The trait
/// uses `async_trait`-free manual desugaring isn't worth it here — the trait
/// is internal and has exactly one real impl.
pub trait KernelConnection: Send {
    /// Launch a kernel process and return a command receiver.
    ///
    /// The command receiver carries `QueueCommand`s from spawned IO tasks
    /// (iopub listener, shell reader, heartbeat monitor) back to the agent's
    /// select loop.
    fn launch(
        config: KernelLaunchConfig,
        shared: KernelSharedRefs,
    ) -> impl std::future::Future<Output = Result<(Self, mpsc::Receiver<QueueCommand>)>> + Send
    where
        Self: Sized;

    /// Send an execute_request to the kernel via the shell channel.
    fn execute(
        &mut self,
        cell_id: &str,
        execution_id: &str,
        source: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Send an interrupt_request via the control channel (SIGINT).
    fn interrupt(&mut self) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Gracefully shut down the kernel process.
    fn shutdown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Forward a raw comm_msg envelope to the kernel (widget interactions).
    fn send_comm_message(
        &mut self,
        raw_message: serde_json::Value,
    ) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Send a comm_msg(update) to sync widget state from frontend to kernel.
    fn send_comm_update(
        &mut self,
        comm_id: &str,
        state: serde_json::Value,
    ) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Request code completions from the kernel.
    ///
    /// Takes `&mut self` because it sends via the primary shell connection.
    /// This is safe because the agent's select! loop is the sole caller.
    fn complete(
        &mut self,
        code: &str,
        cursor_pos: usize,
    ) -> impl std::future::Future<Output = Result<(Vec<CompletionItem>, usize, usize)>> + Send;

    /// Search kernel input history.
    ///
    /// Takes `&mut self` because it sends via the primary shell connection.
    /// This is safe because the agent's select! loop is the sole caller.
    fn get_history(
        &mut self,
        pattern: Option<&str>,
        n: i32,
        unique: bool,
    ) -> impl std::future::Future<Output = Result<Vec<HistoryEntry>>> + Send;

    // ── Read-only metadata accessors ──────────────────────────────────────

    /// Kernel type identifier (e.g., "python", "deno").
    fn kernel_type(&self) -> &str;

    /// Environment source label (e.g., "uv:inline", "conda:prewarmed").
    fn env_source(&self) -> &str;

    /// Environment configuration snapshot from launch time.
    fn launched_config(&self) -> &LaunchedEnvConfig;

    /// Path to the venv directory backing this kernel, if any.
    fn env_path(&self) -> Option<&PathBuf>;

    /// Whether the shell channel connection is active.
    fn is_connected(&self) -> bool;

    // ── Mutable metadata update ───────────────────────────────────────────

    /// Update the UV deps in the launched config after hot-sync.
    ///
    /// Ensures future sync-drift checks reflect the newly installed packages.
    fn update_launched_uv_deps(&mut self, deps: Vec<String>);
}
