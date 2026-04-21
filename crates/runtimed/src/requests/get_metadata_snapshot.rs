//! `NotebookRequest::GetMetadataSnapshot` handler.

use crate::notebook_sync_server::NotebookRoom;
use crate::protocol::NotebookResponse;

pub(crate) async fn handle(room: &NotebookRoom) -> NotebookResponse {
    let doc = room.doc.read().await;
    let snapshot = doc
        .get_metadata_snapshot()
        .and_then(|s| serde_json::to_string(&s).ok());
    NotebookResponse::MetadataSnapshot { snapshot }
}
