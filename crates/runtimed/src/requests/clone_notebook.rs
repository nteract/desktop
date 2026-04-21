//! `NotebookRequest::CloneNotebook` handler.

use crate::notebook_sync_server::{clone_notebook_to_disk, NotebookRoom};
use crate::protocol::NotebookResponse;

pub(crate) async fn handle(room: &NotebookRoom, path: String) -> NotebookResponse {
    match clone_notebook_to_disk(room, &path).await {
        Ok(cloned_path) => NotebookResponse::NotebookCloned { path: cloned_path },
        Err(e) => NotebookResponse::Error {
            error: format!("Failed to clone notebook: {e}"),
        },
    }
}
