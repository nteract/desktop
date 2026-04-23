//! `NotebookRequest::ShutdownKernel` handler.

use runtime_doc::RuntimeLifecycle;

use crate::notebook_sync_server::{send_runtime_agent_request, NotebookRoom};
use crate::protocol::NotebookResponse;

pub(crate) async fn handle(room: &NotebookRoom) -> NotebookResponse {
    // Send shutdown RPC but keep the runtime agent alive — it stays
    // connected for potential RestartKernel. The kernel process dies
    // but the runtime agent subprocess and socket connection remain.
    let has_runtime_agent = room.runtime_agent_request_tx.lock().await.is_some();
    if has_runtime_agent {
        let _ = send_runtime_agent_request(
            room,
            notebook_protocol::protocol::RuntimeAgentRequest::ShutdownKernel,
        )
        .await;
        // Keep runtime agent alive (runtime_agent_handle + runtime_agent_request_tx stay set)
        // so LaunchKernel can send RestartKernel. ExecuteCell/RunAllCells
        // check kernel.lifecycle from RuntimeStateDoc and return NoKernel
        // when it's Shutdown.
        //
        // Update RuntimeStateDoc to reflect shutdown.
        if let Err(e) = room.state.with_doc(|sd| {
            sd.set_lifecycle(&RuntimeLifecycle::Shutdown)?;
            sd.set_queue(None, &[])?;
            Ok(())
        }) {
            tracing::warn!("[runtime-state] {}", e);
        }
        NotebookResponse::KernelShuttingDown {}
    } else {
        NotebookResponse::NoKernel {}
    }
}
