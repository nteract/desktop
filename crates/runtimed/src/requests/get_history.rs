//! `NotebookRequest::GetHistory` handler.

use crate::notebook_sync_server::{send_runtime_agent_request, NotebookRoom};
use crate::protocol::NotebookResponse;

pub(crate) async fn handle(
    room: &NotebookRoom,
    pattern: Option<String>,
    n: i32,
    unique: bool,
) -> NotebookResponse {
    // Agent path: forward via RPC
    let has_runtime_agent = room.runtime_agent_request_tx.lock().await.is_some();
    if has_runtime_agent {
        match send_runtime_agent_request(
            room,
            notebook_protocol::protocol::RuntimeAgentRequest::GetHistory {
                pattern: pattern.clone(),
                n,
                unique,
            },
        )
        .await
        {
            Ok(notebook_protocol::protocol::RuntimeAgentResponse::HistoryResult { entries }) => {
                NotebookResponse::HistoryResult { entries }
            }
            Ok(notebook_protocol::protocol::RuntimeAgentResponse::Error { error }) => {
                NotebookResponse::Error { error }
            }
            Ok(_) => NotebookResponse::Error {
                error: "Unexpected runtime agent response".to_string(),
            },
            Err(e) => NotebookResponse::Error {
                error: format!("Agent error: {}", e),
            },
        }
    } else {
        NotebookResponse::NoKernel {}
    }
}
