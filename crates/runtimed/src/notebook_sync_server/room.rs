use super::*;

/// Per-room identity and filesystem bindings.
///
/// Groups the four fields that describe *which* notebook this room represents,
/// separate from its document state, broadcasts, or kernel lifecycle. These
/// fields are read from most code paths but mutated rarely (path changes when
/// an untitled notebook is saved; working_dir is set once at create time).
pub struct RoomIdentity {
    /// Persistence path for this room's Automerge document (not the .ipynb).
    pub persist_path: PathBuf,
    /// Whether this notebook is ephemeral (in-memory only, no .ipynb on disk).
    pub is_ephemeral: AtomicBool,
    /// The `.ipynb` path, when this room is file-backed. `None` for untitled
    /// and ephemeral rooms. Mutated when an untitled room is saved to disk
    /// (see `handle_save_notebook`).
    pub path: RwLock<Option<PathBuf>>,
    /// Working directory for untitled notebooks (used for project file detection).
    /// When the notebook_id is a UUID (untitled), this provides the directory
    /// context for finding pyproject.toml, pixi.toml, or environment.yaml.
    pub working_dir: RwLock<Option<PathBuf>>,
}

impl RoomIdentity {
    pub fn new(persist_path: PathBuf, path: Option<PathBuf>, ephemeral: bool) -> Self {
        Self {
            persist_path,
            is_ephemeral: AtomicBool::new(ephemeral),
            path: RwLock::new(path),
            working_dir: RwLock::new(None),
        }
    }
}

/// Per-room broadcast fan-out.
///
/// Groups the four channels that distribute room-scoped events to peer sync
/// loops: document-change notifications, kernel broadcasts (EnvProgress,
/// Comm), and presence traffic. `presence`
/// holds the per-peer state that `presence_tx` relays between connections.
pub struct RoomBroadcasts {
    /// Broadcast channel to notify all peers in this room of doc changes.
    pub changed_tx: broadcast::Sender<()>,
    /// Broadcast channel to notify autosave that runtime state changed data
    /// serialized into the `.ipynb` file, such as cell outputs or execution
    /// counts. This deliberately excludes generic RuntimeStateDoc updates like
    /// lifecycle, project context, path, and last_saved.
    pub file_dirty_tx: broadcast::Sender<()>,
    /// Broadcast channel for kernel events: EnvProgress and Comm (widget messages).
    pub kernel_broadcast_tx: broadcast::Sender<NotebookBroadcast>,
    /// Broadcast channel for presence frames (cursor, selection, kernel state).
    /// Carries raw presence bytes plus the peer_id to relay to other peers.
    pub presence_tx: broadcast::Sender<(String, Vec<u8>)>,
    /// Transient peer state (cursors, selections, kernel status).
    /// Protected by RwLock for concurrent reads from multiple peer loops.
    pub presence: Arc<RwLock<PresenceState>>,
}

impl Default for RoomBroadcasts {
    fn default() -> Self {
        let (changed_tx, _) = broadcast::channel(16);
        let (file_dirty_tx, _) = broadcast::channel(16);
        let (kernel_broadcast_tx, _) = broadcast::channel(KERNEL_BROADCAST_CAPACITY);
        let (presence_tx, _) = broadcast::channel(64);
        Self {
            changed_tx,
            file_dirty_tx,
            kernel_broadcast_tx,
            presence_tx,
            presence: Arc::new(RwLock::new(PresenceState::new())),
        }
    }
}

/// Per-room persistence bookkeeping.
///
/// Always present on every room. The optional `debouncer` field nests the
/// two debouncer channels that only exist for non-ephemeral rooms
/// (untitled-saved or file-backed); everything else (save-baseline
/// snapshots, file-watcher shutdown, streaming-load gate, attachment
/// cache) is needed whether the room is ephemeral today or will be
/// promoted to file-backed later.
///
/// When an ephemeral room is saved via `finalize_untitled_promotion`, the
/// watcher shutdown sender and save baselines start getting populated
/// without any struct-level resurrection — the fields were waiting for
/// the file-backed state to arrive.
pub struct RoomPersistence {
    /// Debouncer channels — present only when the room writes to a
    /// persisted Automerge doc (`notebook-docs/*.automerge`). Ephemeral
    /// rooms keep this `None`, and so do rooms promoted via Save (the
    /// `.automerge` stream isn't restarted post-promotion — see comment
    /// in `finalize_untitled_promotion`).
    pub debouncer: Option<PersistDebouncer>,
    /// Cell sources as they were written to disk at last save.
    ///
    /// The file watcher compares disk content against this snapshot (not the
    /// live CRDT) to distinguish our own autosave writes from genuine external
    /// changes (git pull, external editor).
    pub last_save_sources: RwLock<HashMap<String, String>>,
    /// Timestamp (ms since epoch) of last self-write to the .ipynb file.
    /// Used to skip file watcher events triggered by our own saves.
    pub last_self_write: AtomicU64,
    /// Shutdown signal for the file watcher task.
    /// Sent when the room is evicted to stop the watcher.
    pub watcher_shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
    /// Shutdown/flush request channel for the `.ipynb` autosave task.
    ///
    /// The autosave task owns an `Arc<NotebookRoom>`, so broadcast-channel
    /// closure is not a reliable room-lifecycle signal. Eviction uses this
    /// channel to request one acknowledged final save before the task exits.
    pub autosave_shutdown_tx: Mutex<Option<mpsc::UnboundedSender<AutosaveShutdownRequest>>>,
    /// Shutdown signal for the project-file watcher task.
    ///
    /// Separate from `watcher_shutdown_tx` (which watches the `.ipynb`)
    /// because the project-file watcher is pinned to a different path
    /// and can be swapped on save-as without bouncing the notebook
    /// watcher. `None` until `refresh_project_context` finds a project
    /// file and the watcher gets armed.
    pub project_file_watcher_shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
    /// Raw nbformat attachments preserved from disk, keyed by cell ID.
    ///
    /// Populated only by `.ipynb` load paths. Resolved image data already
    /// lives in the Automerge doc as blob URLs per cell; this map is the
    /// raw base64 payload kept in memory so save can round-trip back to
    /// `.ipynb` byte-identically without re-encoding the blob bytes.
    ///
    /// Transitional: this should eventually live in the doc alongside
    /// cells (as blob references with a stable schema) so user-authored
    /// attachments become first-class CRDT data. When that lands, this
    /// field is deleted outright rather than relocated.
    pub nbformat_attachments: RwLock<HashMap<String, serde_json::Value>>,
    /// Whether a streaming load is in progress for this room.
    /// Prevents two connections from both attempting to load from disk.
    is_loading: AtomicBool,
}

/// The debounced `.automerge` persist channels. See `spawn_persist_debouncer`.
pub struct PersistDebouncer {
    /// Channel to send doc bytes to the debounced persistence task.
    /// Uses watch for "latest value" semantics — always keeps most recent state.
    pub persist_tx: watch::Sender<Option<Vec<u8>>>,
    /// Channel to request a synchronous flush from the persist debouncer.
    /// Receiver handles the request and replies on the oneshot after the write
    /// completes. Used by room eviction to guarantee disk consistency *before*
    /// the room is removed from the map, closing the race where a fast reconnect
    /// would load stale bytes from the still-pending .automerge file.
    pub flush_request_tx: mpsc::UnboundedSender<FlushRequest>,
}

impl RoomPersistence {
    /// Build a persistence struct with no active debouncer (ephemeral rooms).
    pub fn ephemeral() -> Self {
        Self {
            debouncer: None,
            last_save_sources: RwLock::new(HashMap::new()),
            last_self_write: AtomicU64::new(0),
            watcher_shutdown_tx: Mutex::new(None),
            autosave_shutdown_tx: Mutex::new(None),
            project_file_watcher_shutdown_tx: Mutex::new(None),
            nbformat_attachments: RwLock::new(HashMap::new()),
            is_loading: AtomicBool::new(false),
        }
    }

    /// Build a persistence struct with an active .automerge debouncer.
    pub fn with_debouncer(
        persist_tx: watch::Sender<Option<Vec<u8>>>,
        flush_request_tx: mpsc::UnboundedSender<FlushRequest>,
    ) -> Self {
        Self {
            debouncer: Some(PersistDebouncer {
                persist_tx,
                flush_request_tx,
            }),
            last_save_sources: RwLock::new(HashMap::new()),
            last_self_write: AtomicU64::new(0),
            watcher_shutdown_tx: Mutex::new(None),
            autosave_shutdown_tx: Mutex::new(None),
            project_file_watcher_shutdown_tx: Mutex::new(None),
            nbformat_attachments: RwLock::new(HashMap::new()),
            is_loading: AtomicBool::new(false),
        }
    }

    /// Atomically claim the loading role. Returns `true` if this caller won
    /// the race and should perform the streaming load.
    pub fn try_start_loading(&self) -> bool {
        self.is_loading
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Mark loading complete (success or failure).
    pub fn finish_loading(&self) {
        self.is_loading.store(false, Ordering::Release);
    }

    /// True if a streaming load is currently in progress.
    pub fn is_loading(&self) -> bool {
        self.is_loading.load(Ordering::Acquire)
    }
}

/// Per-connection accounting for room eviction + `is_draining` reporting.
///
/// - `active_peers`: live counter, drives room eviction when it hits zero.
/// - `had_peers`: one-way latch flipped on first connect. Kept because the
///   Python SDK's `is_draining = (active_peers == 0 && had_peers)` check
///   needs to distinguish "brand-new, no one has connected yet" from
///   "drained, awaiting eviction." Exposed on the `RoomInfo` wire type.
pub struct RoomConnections {
    pub active_peers: AtomicUsize,
    pub had_peers: AtomicBool,
}

impl Default for RoomConnections {
    fn default() -> Self {
        Self {
            active_peers: AtomicUsize::new(0),
            had_peers: AtomicBool::new(false),
        }
    }
}

pub struct NotebookRoom {
    /// Permanent, immutable UUID for this room, independent of the display
    /// path or string lookup keys used by callers.
    pub id: uuid::Uuid,
    /// The canonical Automerge notebook document.
    pub doc: Arc<RwLock<NotebookDoc>>,
    /// Broadcast channels + presence state for fan-out to peer sync loops.
    pub broadcasts: RoomBroadcasts,
    /// Disk persistence state. Always present; ephemeral rooms carry an
    /// `ephemeral()` instance with no active debouncer, and gain the
    /// file watcher + save-baseline tracking when promoted via Save.
    pub persistence: RoomPersistence,
    /// Notebook identity: persist_path, is_ephemeral, .ipynb path, working_dir.
    pub identity: RoomIdentity,
    /// Per-connection accounting: active_peers + had_peers.
    pub connections: RoomConnections,
    /// Blob store for output manifests.
    pub blob_store: Arc<BlobStore>,
    /// Trust state for this notebook (for auto-launch decisions).
    pub trust_state: Arc<RwLock<TrustState>>,
    /// Daemon-local package allowlist for familiar dependency auto-approval.
    pub trusted_packages: crate::trusted_packages::TrustedPackageStore,
    /// Per-notebook RuntimeStateDoc handle — daemon-authoritative ephemeral state
    /// (kernel status, queue, env sync). Clients sync read-only.
    /// Uses `std::sync::Mutex` internally (no `.await` needed).
    pub state: runtime_doc::RuntimeStateHandle,
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
    #[cfg(test)]
    pub fn new_fresh(
        uuid: uuid::Uuid,
        path: Option<PathBuf>,
        docs_dir: &Path,
        blob_store: Arc<BlobStore>,
        ephemeral: bool,
    ) -> Self {
        Self::new_fresh_with_trusted_packages(
            uuid,
            path,
            docs_dir,
            blob_store,
            ephemeral,
            crate::trusted_packages::TrustedPackageStore::unavailable("not configured"),
        )
    }

    pub fn new_fresh_with_trusted_packages(
        uuid: uuid::Uuid,
        path: Option<PathBuf>,
        docs_dir: &Path,
        blob_store: Arc<BlobStore>,
        ephemeral: bool,
        trusted_packages: crate::trusted_packages::TrustedPackageStore,
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
                        approved_uv_dependencies: vec![],
                        conda_dependencies: vec![],
                        approved_conda_dependencies: vec![],
                        conda_channels: vec![],
                        pixi_dependencies: vec![],
                        approved_pixi_dependencies: vec![],
                        pixi_pypi_dependencies: vec![],
                        approved_pixi_pypi_dependencies: vec![],
                        pixi_channels: vec![],
                    },
                    pending_launch: false,
                },
            },
            Some(p) => {
                let mut initial = verify_trust_from_file(p);
                // #2150 reconciliation: if the .ipynb on disk has deps that
                // exactly match a project file's deps (pyproject/env.yml),
                // treat it as Trusted in memory so the auto-launch gate in
                // peer_connection.rs does not block. The signature lands in the doc
                // via streaming_load's reconciliation pass, which fires
                // before the first sync flush.
                if matches!(initial.status, runt_trust::TrustStatus::Untrusted)
                    && project_file_deps_match_trust_info(p, &initial.info)
                {
                    info!(
                        "[notebook-sync] Reconciled project-file trust for {:?} (deps match; promoting Untrusted -> Trusted)",
                        p
                    );
                    initial.status = runt_trust::TrustStatus::Trusted;
                    initial.info.status = runt_trust::TrustStatus::Trusted;
                }
                initial
            }
        };
        info!(
            "[notebook-sync] Trust status for {}: {:?}",
            notebook_id_str, trust_state.status
        );

        let (state_changed_tx, _) = broadcast::channel(16);
        let state = runtime_doc::RuntimeStateHandle::new(RuntimeStateDoc::new(), state_changed_tx);

        // Seed path on the runtime-state doc so connecting peers see it via sync.
        if let Some(p) = path.as_ref() {
            let path_str = p.to_string_lossy().into_owned();
            let _ = state.with_doc(|sd| sd.set_path(Some(&path_str)));
        }

        let persistence = match persist_tx.zip(flush_request_tx) {
            Some((p, f)) => RoomPersistence::with_debouncer(p, f),
            None => RoomPersistence::ephemeral(),
        };

        Self {
            id,
            doc: Arc::new(RwLock::new(doc)),
            broadcasts: RoomBroadcasts::default(),
            persistence,
            identity: RoomIdentity::new(persist_path, path, ephemeral),
            connections: RoomConnections::default(),
            blob_store,
            trust_state: Arc::new(RwLock::new(trust_state)),
            trusted_packages,
            state,
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
        let (persist_tx, persist_rx) = watch::channel::<Option<Vec<u8>>>(None);
        let (flush_request_tx, flush_rx) = mpsc::unbounded_channel::<FlushRequest>();
        spawn_persist_debouncer(persist_rx, flush_rx, persist_path.clone());
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
                        approved_uv_dependencies: vec![],
                        conda_dependencies: vec![],
                        approved_conda_dependencies: vec![],
                        conda_channels: vec![],
                        pixi_dependencies: vec![],
                        approved_pixi_dependencies: vec![],
                        pixi_pypi_dependencies: vec![],
                        approved_pixi_pypi_dependencies: vec![],
                        pixi_channels: vec![],
                    },
                    pending_launch: false,
                },
            },
            Some(p) => {
                let mut initial = verify_trust_from_file(p);
                if matches!(initial.status, runt_trust::TrustStatus::Untrusted)
                    && project_file_deps_match_trust_info(p, &initial.info)
                {
                    initial.status = runt_trust::TrustStatus::Trusted;
                    initial.info.status = runt_trust::TrustStatus::Trusted;
                }
                initial
            }
        };
        let (state_changed_tx, _) = broadcast::channel(16);
        let state = runtime_doc::RuntimeStateHandle::new(RuntimeStateDoc::new(), state_changed_tx);
        if let Some(p) = path.as_ref() {
            let path_str = p.to_string_lossy().into_owned();
            let _ = state.with_doc(|sd| sd.set_path(Some(&path_str)));
        }
        Self {
            id,
            doc: Arc::new(RwLock::new(doc)),
            broadcasts: RoomBroadcasts::default(),
            persistence: RoomPersistence::with_debouncer(persist_tx, flush_request_tx),
            identity: RoomIdentity::new(persist_path, path, false),
            connections: RoomConnections::default(),
            blob_store,
            trust_state: Arc::new(RwLock::new(trust_state)),
            trusted_packages: crate::trusted_packages::TrustedPackageStore::unavailable(
                "not configured",
            ),
            state,
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

    /// Snapshot of nbformat attachments. Empty before the first `.ipynb`
    /// load populates the cache.
    pub async fn nbformat_attachments_snapshot(&self) -> HashMap<String, serde_json::Value> {
        self.persistence.nbformat_attachments.read().await.clone()
    }

    /// Snapshot of cell sources as they were at last save. Empty before
    /// the first save, which is the correct baseline for "no disk write
    /// has happened yet."
    pub async fn last_save_sources_snapshot(&self) -> HashMap<String, String> {
        self.persistence.last_save_sources.read().await.clone()
    }

    /// True if a streaming load is currently in progress.
    pub fn is_loading(&self) -> bool {
        self.persistence.is_loading()
    }

    /// Atomically claim the streaming-load role. Returns `true` if the
    /// caller won the race and should perform the load.
    pub fn try_start_loading(&self) -> bool {
        self.persistence.try_start_loading()
    }

    /// Mark the streaming load complete.
    pub fn finish_loading(&self) {
        self.persistence.finish_loading();
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
            let info = self.state.read(|sd| {
                let state = sd.read_state();
                // The daemon-info NotebookKernelInfo.status field is still a
                // legacy string; derive it from the typed lifecycle via
                // to_legacy so the daemon-info contract is unchanged.
                if !matches!(
                    state.kernel.lifecycle,
                    runtime_doc::RuntimeLifecycle::NotStarted
                ) {
                    let (legacy_status, _phase) = state.kernel.lifecycle.to_legacy();
                    Some((
                        state.kernel.name.clone(),
                        state.kernel.env_source.clone(),
                        legacy_status.to_string(),
                    ))
                } else {
                    None
                }
            });
            if let Ok(Some(info)) = info {
                return Some(info);
            }
        }
        None
    }
}
