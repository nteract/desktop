//! `NotebookRequest::GetDocBytes` handler.

use crate::notebook_sync_server::NotebookRoom;
use crate::protocol::NotebookResponse;

pub(crate) async fn handle(room: &NotebookRoom) -> NotebookResponse {
    let mut doc = room.doc.write().await;
    let bytes = doc.save();
    NotebookResponse::DocBytes { bytes }
}
