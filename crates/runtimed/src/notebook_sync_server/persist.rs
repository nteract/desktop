use super::*;

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
        "[save] save_notebook_to_disk entered: target_path={:?}, room.id={}, room.identity.path={:?}",
        target_path,
        room.id,
        room.identity.path.read().await.as_deref()
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
        None => match room.identity.path.read().await.clone() {
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

    // Read existing .ipynb as raw bytes. Used for two things: the
    // content-hash guard further down (skip no-op writes), and the
    // `nbformat_minor` floor (not carried in the doc today).
    // We no longer read metadata from disk — the doc carries unknown
    // top-level keys as extras, so everything round-trips through
    // the snapshot.
    let existing_raw: Option<Vec<u8>> = match tokio::fs::read(&notebook_path).await {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            warn!(
                "[notebook-sync] Failed to read existing notebook {:?}: {}, \
                 will create new",
                notebook_path, e
            );
            None
        }
    };

    // Read cells, metadata, and per-cell execution_ids from the doc.
    let (cells, metadata_snapshot, cell_execution_ids) = {
        let doc = room.doc.write().await;
        let cells = doc.get_cells();
        let metadata_snapshot = doc.get_metadata_snapshot();
        // Collect execution_id for each cell (for output lookup in state doc)
        let eids: HashMap<String, Option<String>> = cells
            .iter()
            .map(|c| (c.id.clone(), doc.get_execution_id(&c.id)))
            .collect();
        (cells, metadata_snapshot, eids)
    };

    // Read outputs and execution_count from RuntimeStateDoc keyed by execution_id.
    //
    // When a cell is re-queued, `set_execution_id` rewrites the cell's pointer
    // to the new execution before the kernel produces outputs. Saving with the
    // queued/running eid would clobber the previous outputs with an empty list.
    // Fall back to the most recent terminal execution for the cell so an
    // explicit Cmd+S during a run (or a racing autosave) preserves what's on
    // disk until the live run completes. Cleared cells (`execution_id = None`)
    // skip the fall-back and write empty outputs.
    let (cell_outputs, cell_execution_counts): (
        HashMap<String, Vec<serde_json::Value>>,
        HashMap<String, Option<i64>>,
    ) = room
        .state
        .read(|sd| {
            let snapshot = sd.read_state();
            let mut outputs_map = HashMap::new();
            let mut ec_map = HashMap::new();
            for (cell_id, eid) in &cell_execution_ids {
                let Some(eid) = eid.as_ref() else { continue };
                let resolved_eid = resolve_save_execution_id(&snapshot, cell_id, eid);
                let outputs = sd.get_outputs(&resolved_eid);
                if !outputs.is_empty() {
                    outputs_map.insert(cell_id.clone(), outputs);
                }
                if let Some(exec) = snapshot.executions.get(&resolved_eid) {
                    ec_map.insert(cell_id.clone(), exec.execution_count);
                }
            }
            (outputs_map, ec_map)
        })
        .unwrap_or_default();

    let nbformat_attachments = room.nbformat_attachments_snapshot().await;

    // Resolve outputs from the blob store. `resolve_cell_output` returns
    // Jupyter-shape JSON (daemon-runtime shape: includes `output_id`, etc.).
    // The nbformat conversion layer strips runtime-only fields before
    // handing values to the typed v4 deserializer.
    let mut resolved_outputs_by_cell: HashMap<String, Vec<serde_json::Value>> =
        HashMap::with_capacity(cells.len());
    for cell in &cells {
        if cell.cell_type != "code" {
            continue;
        }
        let Some(outputs) = cell_outputs.get(&cell.id) else {
            continue;
        };
        let mut resolved = Vec::with_capacity(outputs.len());
        for output in outputs {
            let output_value = resolve_cell_output(output, &room.blob_store).await;
            resolved.push(output_value);
        }
        resolved_outputs_by_cell.insert(cell.id.clone(), resolved);
    }

    // Metadata comes entirely from the doc. No disk rescue; the
    // snapshot carries unknown keys as extras.
    let metadata = metadata_snapshot
        .as_ref()
        .map(|s| serde_json::to_value(s).unwrap_or_else(|_| serde_json::json!({})))
        .unwrap_or_else(|| serde_json::json!({}));

    // We always write cell IDs, so nbformat_minor is at least 5.
    let existing_minor = existing_raw
        .as_ref()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(bytes).ok())
        .as_ref()
        .and_then(|nb| nb.get("nbformat_minor"))
        .and_then(|v| v.as_u64())
        .unwrap_or(5) as i32;
    let nbformat_minor = std::cmp::max(existing_minor, 5);

    // Build a typed v4::Notebook. The conversion layer enforces nbformat
    // schema invariants (cell id validity, output typing, metadata shape) —
    // structural bugs become compile/deserialize errors instead of silent
    // on-disk drift.
    let v4_notebook = build_v4_notebook(
        &cells,
        &resolved_outputs_by_cell,
        &cell_execution_counts,
        &nbformat_attachments,
        &metadata,
        nbformat_minor,
    )
    .map_err(|e| SaveError::Unrecoverable(format!("Failed to build v4 notebook: {e}")))?;
    let cell_count = v4_notebook.cells.len();

    // Collect raw-cell attachments (markdown attachments are already on the
    // typed v4::Cell::Markdown variant; raw cells lose theirs in typed
    // conversion and get re-injected during serialize).
    let raw_attachments: HashMap<String, serde_json::Value> = cells
        .iter()
        .filter(|c| c.cell_type == "raw")
        .filter_map(|c| {
            nbformat_attachments
                .get(&c.id)
                .map(|att| (c.id.clone(), att.clone()))
        })
        .collect();

    let content_with_newline = serialize_v4_notebook(&v4_notebook, &raw_attachments)
        .map_err(|e| SaveError::Retryable(format!("Failed to serialize notebook: {e}")))?;

    // Content-hash guard: skip the write if the serialized bytes match what is
    // already on disk. Prevents no-op autosaves from dirtying the working tree.
    if let Some(ref raw) = existing_raw {
        if raw.as_slice() == content_with_newline.as_bytes() {
            debug!(
                "[notebook-sync] Skipping write - content unchanged for {:?}",
                notebook_path
            );
            // Still update save baselines so the file watcher stays consistent.
            let is_primary_path = target_path.is_none()
                || room.identity.path.read().await.as_deref() == Some(notebook_path.as_path());
            if is_primary_path {
                let mut saved = HashMap::with_capacity(cells.len());
                for cell in &cells {
                    saved.insert(cell.id.clone(), cell.source.clone());
                }
                *room.persistence.last_save_sources.write().await = saved;
            }
            return Ok(notebook_path.to_string_lossy().to_string());
        }
    }

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
    tokio::fs::write(&notebook_path, &content_with_newline)
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

    // Update last_self_write timestamp so the file watcher skips our own write.
    // Applies to all rooms (including ephemeral that were just promoted to
    // file-backed via this save) - a watcher may start up right after
    // `finalize_untitled_promotion` and will consult this baseline.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    room.persistence
        .last_self_write
        .store(now, Ordering::Relaxed);

    // Snapshot cell sources at save time so the file watcher can distinguish
    // our own writes from genuine external changes. Only update when saving
    // to the primary path - saving to an alternate path (Save As) must not
    // corrupt the baseline for the file watcher.
    let is_primary_path = target_path.is_none()
        || room.identity.path.read().await.as_deref() == Some(notebook_path.as_path());
    if is_primary_path {
        let mut saved = HashMap::with_capacity(cells.len());
        for cell in &cells {
            saved.insert(cell.id.clone(), cell.source.clone());
        }
        *room.persistence.last_save_sources.write().await = saved;
    }

    info!(
        "[notebook-sync] Saved notebook to disk: {:?} ({} cells)",
        notebook_path, cell_count
    );

    Ok(notebook_path.to_string_lossy().to_string())
}

/// Transitions an untitled room to file-backed: claims path in path_index,
/// updates room.identity.path, cleans up the stale `.automerge` persist file, spawns
/// the `.ipynb` file watcher and autosave debouncer, clears ephemeral markers,
/// and stamps the new path on the runtime-state doc (peers see it via sync).
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
/// autosave debouncer spawn, ephemeral marker clear, and runtime-state doc
/// `path` write.
pub(crate) async fn finalize_untitled_promotion(room: &Arc<NotebookRoom>, canonical: PathBuf) {
    // Update room's path now that path_index owns it.
    *room.identity.path.write().await = Some(canonical.clone());

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
    if room.identity.persist_path.exists() {
        if let Err(e) = tokio::fs::remove_file(&room.identity.persist_path).await {
            warn!(
                "[notebook-sync] Failed to remove stale persist file {:?}: {}",
                room.identity.persist_path, e
            );
        }
    }

    // Spawn .ipynb file watcher. RoomPersistence is always present, so the
    // shutdown sender finds a home whether the room was born file-backed or
    // promoted from ephemeral.
    if canonical.extension().is_some_and(|ext| ext == "ipynb") {
        let shutdown_tx = spawn_notebook_file_watcher(canonical.clone(), Arc::clone(room));
        *room.persistence.watcher_shutdown_tx.lock().await = Some(shutdown_tx);
    }

    // Spawn autosave debouncer so subsequent edits persist to .ipynb.
    spawn_autosave_debouncer(canonical.to_string_lossy().into_owned(), Arc::clone(room));

    // Clear ephemeral markers.
    room.identity.is_ephemeral.store(false, Ordering::Relaxed);
    {
        let mut doc = room.doc.write().await;
        let _ = doc.delete_metadata("ephemeral");
    }

    // Stamp the new path on the runtime-state doc; peers see it via sync.
    let path_str = canonical.to_string_lossy().into_owned();
    if let Err(e) = room.state.with_doc(|sd| sd.set_path(Some(&path_str))) {
        warn!("[notebook-sync] set_path on promote failed: {}", e);
    }

    // Project context was `Pending` for untitled; now that we have a
    // real path, walk up from it. Same call shape catalog uses on room
    // creation.
    super::project_context::refresh_project_context_async(room, Some(canonical.as_path())).await;

    info!(
        "[notebook-sync] Promoted untitled room {} to file-backed path {:?}",
        room.id, canonical
    );
}

/// Pick which execution_id save should read outputs from.
///
/// If the cell's current execution is queued or running with no outputs yet,
/// fall back to the most recent done/error execution for the same cell. The
/// fall-back applies only when the in-flight execution would yield empty
/// outputs; once any output lands at the new id we use it (partial-but-live
/// is preferred over stale-but-complete the moment the kernel speaks).
fn resolve_save_execution_id(
    snapshot: &runtime_doc::RuntimeState,
    cell_id: &str,
    current_eid: &str,
) -> String {
    let current = snapshot.executions.get(current_eid);
    let in_flight_with_no_outputs = current
        .map(|exec| {
            (exec.status == "queued" || exec.status == "running") && exec.outputs.is_empty()
        })
        .unwrap_or(false);
    if !in_flight_with_no_outputs {
        return current_eid.to_string();
    }
    snapshot
        .executions
        .iter()
        .filter(|(eid, exec)| {
            eid.as_str() != current_eid
                && exec.cell_id == cell_id
                && (exec.status == "done" || exec.status == "error")
        })
        .max_by_key(|(_, exec)| exec.seq.unwrap_or(0))
        .map(|(eid, _)| eid.clone())
        .unwrap_or_else(|| current_eid.to_string())
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
pub(crate) struct PersistDebouncerConfig {
    /// How long to wait after last update before flushing (debounce window)
    pub(crate) debounce_ms: u64,
    /// Maximum time between flushes during continuous updates
    pub(crate) max_interval_ms: u64,
    /// How often to check if we should flush
    pub(crate) check_interval_ms: u64,
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
pub(crate) type FlushRequest = oneshot::Sender<bool>;

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
pub(crate) fn spawn_persist_debouncer(
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
pub(crate) fn spawn_persist_debouncer_with_config(
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
/// whenever serialized notebook content changes. Only for saved (non-untitled)
/// notebooks. Does NOT format cells — formatting is reserved for explicit saves.
///
/// NotebookDoc edits arrive on `changed_tx`; runtime changes that can affect
/// `.ipynb` bytes, such as outputs and execution counts, arrive on
/// `file_dirty_tx`. Generic RuntimeStateDoc broadcasts are intentionally not
/// autosave triggers because they also carry session/UI fields like
/// `last_saved`, lifecycle, path, and project context.
pub(crate) fn spawn_autosave_debouncer(notebook_id: String, room: Arc<NotebookRoom>) {
    spawn_autosave_debouncer_with_config(notebook_id, room, AutosaveDebouncerConfig::default());
}

/// Spawn autosave debouncer with custom timing configuration (for testing).
fn spawn_autosave_debouncer_with_config(
    notebook_id: String,
    room: Arc<NotebookRoom>,
    config: AutosaveDebouncerConfig,
) {
    let mut changed_rx = room.broadcasts.changed_tx.subscribe();
    let mut file_dirty_rx = room.broadcasts.file_dirty_tx.subscribe();
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
                                    && !room.is_loading()
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
                    file_dirty_result = file_dirty_rx.recv() => {
                        match file_dirty_result {
                            Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {
                                last_receive = Some(Instant::now());
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                // Room teardown is already represented by
                                // changed_rx closing. Stop watching this
                                // auxiliary signal if it disappears first.
                                let (_tx, rx) = broadcast::channel(1);
                                file_dirty_rx = rx;
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
                            // Skip during initial load. Also clear last_receive
                            // so load-time change notifications don't trigger a
                            // save the moment loading completes.
                            if room.is_loading() {
                                last_receive = None;
                                continue;
                            }

                            match save_notebook_to_disk(&room, None).await {
                                Ok(path) => {
                                    debug!("[autosave] Saved {}", path);
                                    last_flush = Some(Instant::now());

                                    // Check if changes arrived during the save. If so,
                                    // keep last_receive set so we flush again soon —
                                    // don't stamp last_saved while the file is stale.
                                    let changed_during_save =
                                        matches!(changed_rx.try_recv(), Ok(()) | Err(broadcast::error::TryRecvError::Lagged(_)));
                                    let file_dirty_during_save =
                                        matches!(file_dirty_rx.try_recv(), Ok(()) | Err(broadcast::error::TryRecvError::Lagged(_)));
                                    if changed_during_save || file_dirty_during_save {
                                        last_receive = Some(Instant::now());
                                    } else {
                                        last_receive = None;
                                        // Stamp last_saved on the runtime-state doc.
                                        // Frontends compute dirty = local_edit_at > last_saved.
                                        let now = chrono::Utc::now().to_rfc3339();
                                        if let Err(e) = room.state.with_doc(|sd| {
                                            sd.set_last_saved(Some(&now))
                                        }) {
                                            warn!("[autosave] set_last_saved failed: {}", e);
                                        }
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
pub(crate) const SELF_WRITE_SKIP_WINDOW_MS: u64 = 600;

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

                            // Check if this is a self-write (within skip window of our last save).
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0);
                            let last_write = room.persistence.last_self_write.load(Ordering::Relaxed);
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
                                attachments: external_attachments,
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
                                let _ = room.broadcasts.changed_tx.send(());
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
