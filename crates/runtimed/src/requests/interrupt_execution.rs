//! `NotebookRequest::InterruptExecution` handler.

use crate::notebook_sync_server::{send_runtime_agent_command, NotebookRoom};
use crate::protocol::NotebookResponse;

pub(crate) async fn handle(room: &NotebookRoom) -> NotebookResponse {
    let has_runtime_agent = room.runtime_agent_request_tx.lock().await.is_some();
    if has_runtime_agent {
        // Do NOT mark executions as failed here on the coordinator side.
        // The coordinator's CRDT copy may contain entries from a concurrent
        // ExecuteCell whose final state should be determined by the runtime
        // agent, not pre-empted by a blanket sweep.  The runtime agent's
        // interrupt handler calls mark_inflight_executions_failed() on its
        // own CRDT copy - only entries that have actually synced to the
        // agent are affected, so final state is correct regardless of
        // timing between ExecuteCell and InterruptExecution.
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
