//! `NotebookRequest::InterruptExecution` handler.

use crate::notebook_sync_server::{send_runtime_agent_command, NotebookRoom};
use crate::protocol::NotebookResponse;

pub(crate) async fn handle(room: &NotebookRoom) -> NotebookResponse {
    let has_runtime_agent = room.runtime_agent_request_tx.lock().await.is_some();
    if has_runtime_agent {
        // Do NOT mark executions as failed here on the coordinator side.
        // A concurrent ExecuteCell may have just queued an entry that should
        // run normally after the interrupt completes.  The runtime agent's
        // interrupt handler calls mark_inflight_executions_failed() on its
        // own CRDT copy, which only catches entries that have already synced
        // to the agent (i.e. entries that were genuinely in-flight).  Entries
        // created concurrently arrive in a later sync frame and get picked
        // up by get_queued_executions() for normal execution.
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
