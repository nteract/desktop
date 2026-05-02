//! `NotebookRequest::InterruptExecution` handler.

use tracing::warn;

use crate::notebook_sync_server::{send_runtime_agent_command, NotebookRoom};
use crate::protocol::NotebookResponse;

pub(crate) async fn handle(room: &NotebookRoom) -> NotebookResponse {
    let has_runtime_agent = room.runtime_agent_request_tx.lock().await.is_some();
    if has_runtime_agent {
        // Mark all in-flight executions as failed on the coordinator's copy
        // of RuntimeStateDoc BEFORE sending the interrupt to the runtime
        // agent.  This catches execution entries created by a concurrent
        // ExecuteCell that haven't synced to the runtime agent yet — without
        // this, those entries stay "queued" forever because the runtime
        // agent's local queue doesn't know about them when it clears.
        if let Err(e) = room.state.with_doc(|sd| {
            sd.mark_inflight_executions_failed()?;
            Ok(())
        }) {
            warn!(
                "[interrupt] Failed to mark inflight executions on coordinator: {}",
                e
            );
        }

        // Fire-and-forget: the agent handles the SIGINT signal and updates
        // its local queue / RuntimeStateDoc copy.
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
