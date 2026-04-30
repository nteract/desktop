use super::*;

pub(crate) async fn send_runtime_agent_command(
    room: &NotebookRoom,
    request: notebook_protocol::protocol::RuntimeAgentRequest,
) -> anyhow::Result<()> {
    let tx = {
        let guard = room.runtime_agent_request_tx.lock().await;
        guard
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Runtime agent not connected"))?
    };
    let envelope = notebook_protocol::protocol::RuntimeAgentRequestEnvelope {
        id: uuid::Uuid::new_v4().to_string(),
        request,
    };
    tx.send(RuntimeAgentMessage::Command(envelope))
        .await
        .map_err(|_| anyhow::anyhow!("Runtime agent disconnected"))?;
    Ok(())
}

/// Send a query to the runtime agent and wait for a sync response.
///
/// Only used for Complete and GetHistory which need return values.
pub(crate) async fn send_runtime_agent_query(
    room: &NotebookRoom,
    request: notebook_protocol::protocol::RuntimeAgentRequest,
) -> anyhow::Result<notebook_protocol::protocol::RuntimeAgentResponse> {
    let timeout = runtime_agent_query_timeout(&request);
    let tx = {
        let guard = room.runtime_agent_request_tx.lock().await;
        guard
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Runtime agent not connected"))?
    };
    let envelope = notebook_protocol::protocol::RuntimeAgentRequestEnvelope {
        id: uuid::Uuid::new_v4().to_string(),
        request,
    };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    tx.send(RuntimeAgentMessage::Query(envelope, reply_tx))
        .await
        .map_err(|_| anyhow::anyhow!("Runtime agent disconnected"))?;
    match tokio::time::timeout(timeout, reply_rx).await {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(_)) => Err(anyhow::anyhow!("Runtime agent dropped reply")),
        Err(_) => Err(anyhow::anyhow!("Runtime agent query timed out")),
    }
}

/// Send an RPC request to the runtime agent.
///
/// Routes commands as fire-and-forget, queries as sync RPCs.
/// Callers that don't need a response should use `send_runtime_agent_command` directly.
pub(crate) async fn send_runtime_agent_request(
    room: &NotebookRoom,
    request: notebook_protocol::protocol::RuntimeAgentRequest,
) -> anyhow::Result<notebook_protocol::protocol::RuntimeAgentResponse> {
    if request.is_command() {
        send_runtime_agent_command(room, request).await?;
        Ok(notebook_protocol::protocol::RuntimeAgentResponse::Ok)
    } else {
        send_runtime_agent_query(room, request).await
    }
}

/// Reserve daemon-owned kernel ports, send a launch/restart request, and retry
/// with a fresh reservation if the runtime agent loses a port bind race.
pub(crate) async fn send_runtime_agent_request_with_kernel_ports<F>(
    room: &NotebookRoom,
    mut build_request: F,
) -> anyhow::Result<notebook_protocol::protocol::RuntimeAgentResponse>
where
    F: FnMut(
        notebook_protocol::protocol::KernelPorts,
    ) -> notebook_protocol::protocol::RuntimeAgentRequest,
{
    for attempt in 1..=crate::kernel_ports::MAX_KERNEL_PORT_LAUNCH_ATTEMPTS {
        let port_reservation = crate::kernel_ports::reserve_kernel_ports().await?;
        let response =
            send_runtime_agent_request(room, build_request(port_reservation.ports())).await?;

        match &response {
            notebook_protocol::protocol::RuntimeAgentResponse::Error { error }
                if crate::kernel_ports::is_kernel_port_bind_error(error)
                    && attempt < crate::kernel_ports::MAX_KERNEL_PORT_LAUNCH_ATTEMPTS =>
            {
                warn!(
                    "[notebook-sync] Runtime agent hit kernel port bind race on attempt {}/{}; retrying with fresh ports: {}",
                    attempt,
                    crate::kernel_ports::MAX_KERNEL_PORT_LAUNCH_ATTEMPTS,
                    error
                );
                continue;
            }
            _ => return Ok(response),
        }
    }

    unreachable!("kernel port launch retry loop must return from the final attempt")
}
