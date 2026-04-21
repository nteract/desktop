//! `NotebookRequest::SendComm` handler.

use crate::notebook_sync_server::{send_runtime_agent_request, NotebookRoom};
use crate::protocol::NotebookResponse;

pub(crate) async fn handle(room: &NotebookRoom, message: serde_json::Value) -> NotebookResponse {
    // Agent path: forward comm message via RPC
    let has_runtime_agent = room.runtime_agent_request_tx.lock().await.is_some();
    if has_runtime_agent {
        match send_runtime_agent_request(
            room,
            notebook_protocol::protocol::RuntimeAgentRequest::SendComm {
                message: message.clone(),
            },
        )
        .await
        {
            Ok(_) => NotebookResponse::Ok {},
            Err(e) => NotebookResponse::Error {
                error: format!("Agent comm error: {}", e),
            },
        }
    } else {
        NotebookResponse::NoKernel {}
    }
}
