use super::*;

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

    // Record the notebook's project-file context on the runtime-state doc.
    // Single-writer invariant: only the daemon writes this key. Also
    // re-runs after untitled promotion and save-as rename; see
    // `project_context::refresh_project_context` callers.
    super::project_context::refresh_project_context_async(&room, path.as_deref()).await;

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
            if let Ok(mut guard) = room.persistence.watcher_shutdown_tx.try_lock() {
                *guard = Some(shutdown_tx);
            }
        }

        // Spawn autosave debouncer to keep .ipynb on disk current.
        let path_str = notebook_path.to_string_lossy().to_string();
        spawn_autosave_debouncer(path_str, room.clone());
    }

    room
}
