//! `NotebookRequest::GetKernelInfo` handler.

use crate::notebook_sync_server::NotebookRoom;
use crate::protocol::NotebookResponse;

pub(crate) async fn handle(room: &NotebookRoom) -> NotebookResponse {
    // Read from RuntimeStateDoc (source of truth for runtime agent)
    let sd = room.state_doc.read().await;
    let state = sd.read_state();
    if state.kernel.status != "not_started" && !state.kernel.status.is_empty() {
        NotebookResponse::KernelInfo {
            kernel_type: if state.kernel.name.is_empty() {
                None
            } else {
                Some(state.kernel.name)
            },
            env_source: if state.kernel.env_source.is_empty() {
                None
            } else {
                Some(state.kernel.env_source)
            },
            status: state.kernel.status,
        }
    } else {
        NotebookResponse::KernelInfo {
            kernel_type: None,
            env_source: None,
            status: "not_started".to_string(),
        }
    }
}
