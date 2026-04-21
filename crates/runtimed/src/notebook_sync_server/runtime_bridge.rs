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

/// Send an RPC request to the runtime agent (legacy wrapper).
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
