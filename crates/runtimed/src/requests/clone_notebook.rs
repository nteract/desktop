//! `NotebookRequest::CloneAsEphemeral` handler.
//!
//! Forks a source notebook into a new ephemeral room. The new room has:
//! - Fresh UUID, fresh env_id
//! - All cells, metadata, and markdown attachments from the source
//! - No outputs, execution_count = null on every code cell
//! - trust_signature / trust_timestamp cleared (new notebook, new machine)
//!
//! The room is registered in `daemon.notebook_rooms` before this function
//! returns. A peer can then attach via `Handshake::NotebookSync`.

use std::path::PathBuf;
use std::sync::Arc;

use uuid::Uuid;

use crate::daemon::Daemon;
use crate::notebook_sync_server::{get_or_create_room, NotebookRoom};
use crate::protocol::NotebookResponse;

pub(crate) async fn handle(daemon: &Arc<Daemon>, source_notebook_id: String) -> NotebookResponse {
    // 1. Look up source room.
    let source_uuid = match Uuid::parse_str(&source_notebook_id) {
        Ok(u) => u,
        Err(_) => {
            return NotebookResponse::Error {
                error: format!("Invalid source_notebook_id: {source_notebook_id}"),
            };
        }
    };
    let source_room = {
        let rooms = daemon.notebook_rooms.lock().await;
        match rooms.get(&source_uuid).cloned() {
            Some(r) => r,
            None => {
                return NotebookResponse::Error {
                    error: format!("Source notebook not found: {source_notebook_id}"),
                };
            }
        }
    };

    // 2. Derive working_dir: source_path.parent() ?? source.working_dir.
    let working_dir_path = derive_working_dir(&source_room).await;

    // 3. Mint a fresh UUID for the clone.
    let clone_uuid = Uuid::new_v4();

    // 4. Create the new ephemeral room (empty).
    let clone_room = get_or_create_room(
        &daemon.notebook_rooms,
        &daemon.path_index,
        clone_uuid,
        None, // ephemeral, no file path
        &daemon.config.notebook_docs_dir,
        daemon.blob_store.clone(),
        true, // ephemeral
    )
    .await;

    // 5. Seed the room's working_dir so project-file resolution finds the
    //    same pyproject.toml / environment.yml / pixi.toml the source uses.
    if let Some(ref wd) = working_dir_path {
        *clone_room.identity.working_dir.write().await = Some(wd.clone());
    }

    // 6. Fork cells + metadata + attachments.
    if let Err(e) = seed_clone_from_source(&source_room, &clone_room).await {
        // On seed failure, evict the partially-initialized room so we
        // don't leak an empty ephemeral.
        daemon.notebook_rooms.lock().await.remove(&clone_uuid);
        return NotebookResponse::Error {
            error: format!("Failed to seed cloned notebook: {e}"),
        };
    }

    NotebookResponse::NotebookCloned {
        notebook_id: clone_uuid.to_string(),
        working_dir: working_dir_path.map(|p| p.to_string_lossy().into_owned()),
    }
}

/// Effective working directory for a room: the parent of its .ipynb
/// if file-backed, or the explicit working_dir stored on the room for
/// untitled rooms. None only if both are absent.
async fn derive_working_dir(room: &NotebookRoom) -> Option<PathBuf> {
    if let Some(path) = room.identity.path.read().await.as_ref() {
        if let Some(parent) = path.parent() {
            return Some(parent.to_path_buf());
        }
    }
    room.identity.working_dir.read().await.clone()
}

/// Seed the clone room's Automerge doc from the source, then copy markdown
/// attachments. Called once, immediately after room creation; no other peer
/// can observe the room between `get_or_create_room` and this call.
async fn seed_clone_from_source(
    source: &NotebookRoom,
    clone: &Arc<NotebookRoom>,
) -> Result<(), String> {
    // Snapshot source state in a single lock scope to avoid tearing.
    let (cells, metadata_snapshot) = {
        let doc = source.doc.read().await;
        (doc.get_cells(), doc.get_metadata_snapshot())
    };
    let attachments = source.nbformat_attachments_snapshot().await;

    // Seed the clone's doc.
    {
        let mut clone_doc = clone.doc.write().await;

        for cell in &cells {
            // `add_cell_full` takes execution_count as the JSON-encoded
            // string stored on the Automerge doc. Source/markdown cells
            // naturally carry "null"; for code cells we force "null" here
            // to clear any stale count the source had.
            let encoded_exec_count = if cell.cell_type == "code" {
                "null".to_string()
            } else {
                cell.execution_count.clone()
            };
            clone_doc
                .add_cell_full(
                    &cell.id,
                    &cell.cell_type,
                    &cell.position,
                    &cell.source,
                    &encoded_exec_count,
                    &cell.metadata,
                )
                .map_err(|e| format!("add_cell_full({}): {e}", cell.id))?;
        }

        // Apply metadata with fresh env_id + cleared trust.
        if let Some(mut snapshot) = metadata_snapshot {
            snapshot.runt.env_id = Some(Uuid::new_v4().to_string());
            snapshot.runt.trust_signature = None;
            snapshot.runt.trust_timestamp = None;
            clone_doc
                .set_metadata_snapshot(&snapshot)
                .map_err(|e| format!("set_metadata_snapshot: {e}"))?;
        }

        // Ephemeral marker lives in raw metadata (set by new_fresh already),
        // no action here.
    }

    // Copy the markdown-attachment cache. Raw-cell attachments are included
    // too since nbformat_attachments doesn't discriminate by cell_type; the
    // save path re-injects them for raw cells via the existing
    // nbformat_convert wrapper.
    if !attachments.is_empty() {
        let mut cache = clone.persistence.nbformat_attachments.write().await;
        *cache = attachments;
    }

    Ok(())
}
