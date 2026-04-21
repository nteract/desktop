//! `NotebookRequest::GetRawMetadata` handler.

use crate::notebook_sync_server::NotebookRoom;
use crate::protocol::NotebookResponse;

pub(crate) async fn handle(room: &NotebookRoom, key: String) -> NotebookResponse {
    let doc = room.doc.read().await;
    let value = doc.get_metadata(&key);
    NotebookResponse::RawMetadata { value }
}
