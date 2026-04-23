//! `NotebookRequest::SaveNotebook` handler.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use tracing::warn;

use crate::daemon::Daemon;
use crate::notebook_sync_server::{
    canonical_target_path, finalize_untitled_promotion, format_notebook_cells,
    persist_notebook_bytes, save_notebook_to_disk, try_claim_path, NotebookRoom, SaveError,
};
use crate::protocol::{NotebookBroadcast, NotebookResponse};

pub(crate) async fn handle(
    room: &Arc<NotebookRoom>,
    daemon: &Arc<Daemon>,
    format_cells: bool,
    path: Option<String>,
) -> NotebookResponse {
    // Format cells if requested (before saving)
    if format_cells {
        if let Err(e) = format_notebook_cells(room).await {
            warn!("[save] Format cells failed (continuing with save): {}", e);
        }
    }

    // Capture was_untitled and old_path in a single critical section to
    // avoid a TOCTOU race between the two reads.
    let (was_untitled, old_path) = {
        let p = room.identity.path.read().await;
        (p.is_none(), p.clone())
    };

    // For any save that writes to a NEW path (untitled promotion or
    // save-as rename), claim path_index BEFORE touching disk. Writing
    // first and then checking the claim would overwrite another room's
    // file if both happen to target the same path — the overwritten
    // file then trips the other room's watcher, wiping its CRDT cells.
    //
    // Compute the pre-write canonical target. For untitled rooms a path
    // is required; for file-backed rooms we only need a pre-write claim
    // if the caller specified a path different from room.identity.path.
    let target_for_claim: Option<PathBuf> = match (&path, was_untitled) {
        (Some(p), _) => match crate::paths::normalize_save_target(p) {
            Ok(normalized) => Some(canonical_target_path(&normalized).await),
            Err(msg) => {
                return NotebookResponse::SaveError {
                    error: notebook_protocol::protocol::SaveErrorKind::Io { message: msg },
                };
            }
        },
        (None, true) => {
            // Untitled save with no path — the daemon requires one.
            // Fall through to save_notebook_to_disk which returns the
            // structured error; no claim needed (no write happens).
            None
        }
        (None, false) => None, // save-in-place on file-backed room
    };

    // The new path that needs a pre-write claim (if any). Separates
    // "claim required" from "have a claim path" so downstream branches
    // don't need a runtime is_some + unwrap.
    let pre_claim: Option<PathBuf> = match (&target_for_claim, &old_path) {
        (Some(t), Some(old)) if t != old => Some(t.clone()),
        (Some(t), None) => Some(t.clone()),
        _ => None,
    };

    if let Some(ref canonical_pre) = pre_claim {
        if let Err(kind) = try_claim_path(&daemon.path_index, canonical_pre, room.id).await {
            return NotebookResponse::SaveError { error: kind };
        }
    }

    let written = match save_notebook_to_disk(room, path.as_deref()).await {
        Ok(p) => p,
        Err(e) => {
            // Rollback the path_index claim we just made so the room
            // stays untitled / its old path stays claimed.
            if let Some(ref canonical_pre) = pre_claim {
                daemon.path_index.lock().await.remove(canonical_pre);
            }
            // Emergency persist for ephemeral rooms: if saving to .ipynb
            // failed, at least write the Automerge doc so data isn't lost.
            if room.identity.is_ephemeral.load(Ordering::Relaxed)
                && room.persistence.debouncer.is_none()
            {
                let bytes = room.doc.write().await.save();
                persist_notebook_bytes(&bytes, &room.identity.persist_path);
                warn!(
                    "[notebook-sync] Save failed for ephemeral room — emergency persist to {:?}",
                    room.identity.persist_path
                );
            }
            let kind = match e {
                SaveError::Unrecoverable(msg) | SaveError::Retryable(msg) => {
                    notebook_protocol::protocol::SaveErrorKind::Io { message: msg }
                }
            };
            return NotebookResponse::SaveError { error: kind };
        }
    };

    // Post-write canonicalize. Usually matches the pre-write key. If it
    // differs (uncommon — only when parent-canonicalize disagreed with
    // full canonicalize), swap the path_index entry.
    let canonical = match tokio::fs::canonicalize(&written).await {
        Ok(c) => c,
        Err(e) => {
            warn!(
                "[notebook-sync] post-save canonicalize({}) failed: {} — using raw path. \
                 Duplicate-room detection may be weakened.",
                written, e
            );
            PathBuf::from(&written)
        }
    };

    if let Some(ref canonical_pre) = pre_claim {
        if canonical_pre != &canonical {
            let mut idx = daemon.path_index.lock().await;
            idx.remove(canonical_pre);
            // Best-effort reinsert under the post-write canonical.
            if let Err(e) = idx.insert(canonical.clone(), room.id) {
                warn!(
                    "[notebook-sync] post-write path_index reinsert failed for {:?}: {} \
                     — room {} may be orphaned from path lookup",
                    canonical, e, room.id
                );
            }
        }
    }

    if was_untitled {
        finalize_untitled_promotion(room, canonical.clone()).await;
    } else if let Some(old) = old_path.as_ref() {
        let path_changed = old != &canonical;
        if path_changed {
            // Save-as rename: new path already claimed above; remove
            // the old path_index entry and update room.identity.path.
            {
                let mut idx = daemon.path_index.lock().await;
                idx.remove(old);
            }
            *room.identity.path.write().await = Some(canonical.clone());
            let _ = room
                .broadcasts
                .kernel_broadcast_tx
                .send(NotebookBroadcast::PathChanged {
                    path: Some(canonical.to_string_lossy().into_owned()),
                });
        }
        // If path didn't change, this is save-in-place: nothing else.
    }

    NotebookResponse::NotebookSaved { path: written }
}
