//! `NotebookRequest::SyncEnvironment` handler.
//!
//! Thin wrapper that delegates to `notebook_sync_server::handle_sync_environment`.
//! The actual logic (hot-install packages into the running kernel) is large
//! enough to warrant its own file in a follow-up; the wrapper is here so the
//! dispatcher reads uniformly.

use crate::notebook_sync_server::{handle_sync_environment, NotebookRoom};
use crate::protocol::NotebookResponse;
use crate::requests::guarded;

pub(crate) async fn handle(room: &NotebookRoom) -> NotebookResponse {
    handle_sync_environment(room).await
}

pub(crate) async fn handle_guarded(
    room: &NotebookRoom,
    observed_heads: Vec<String>,
    dependency_fingerprint: String,
) -> NotebookResponse {
    if let Err(rejection) = guarded::ensure_trusted(room).await {
        return rejection.into_response();
    }

    {
        let mut doc = room.doc.write().await;
        if let Err(rejection) =
            guarded::validate_sync_environment(&mut doc, &observed_heads, &dependency_fingerprint)
        {
            return rejection.into_response();
        }
    }

    handle(room).await
}
