//! `NotebookRequest::InterruptExecution` handler.

use crate::notebook_sync_server::{send_runtime_agent_command, NotebookRoom};
use crate::protocol::NotebookResponse;

pub(crate) async fn handle(room: &NotebookRoom) -> NotebookResponse {
    let has_runtime_agent = room.runtime_agent_request_tx.lock().await.is_some();
    if has_runtime_agent {
        // Fire-and-forget: the agent handles interrupt and updates
        // RuntimeStateDoc CRDT directly (clears queue, marks executions).
        match send_runtime_agent_command(
            room,
            notebook_protocol::protocol::RuntimeAgentRequest::InterruptExecution,
        )
        .await
        {
            Ok(()) => NotebookResponse::InterruptSent {},
            Err(e) => NotebookResponse::Error {
                error: format!("Agent interrupt error: {}", e),
            },
        }
    } else {
        NotebookResponse::NoKernel {}
    }
}
