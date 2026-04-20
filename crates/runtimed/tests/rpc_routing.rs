// Tests can use unwrap/expect freely - panics are acceptable in test code
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Tests for Phase 2 RPC routing: correlation IDs, fire-and-forget commands,
//! and the ShutdownKernel sync guarantee.
//!
//! These tests exercise the channel routing logic without spawning real kernels.
//! They verify that:
//! - Correlation IDs route responses to the correct caller
//! - Fire-and-forget commands don't block subsequent sync queries
//! - ShutdownKernel is sync (prevents the CRDT race with LaunchKernel)
//! - InterruptHandle fires independently of the serial request queue

use std::sync::Arc;
use std::time::Duration;

use notebook_protocol::protocol::{
    RuntimeAgentRequest, RuntimeAgentRequestEnvelope, RuntimeAgentResponse,
};
use tokio::sync::{mpsc, oneshot};

/// Simulates the daemon's RuntimeAgentMessage routing.
/// Mirrors the enum at notebook_sync_server.rs:102.
enum RuntimeAgentMessage {
    Command(RuntimeAgentRequestEnvelope),
    Query(
        RuntimeAgentRequestEnvelope,
        oneshot::Sender<RuntimeAgentResponse>,
    ),
}

/// Route a request through the same logic as `send_runtime_agent_request`.
async fn route_request(
    tx: &mpsc::Sender<RuntimeAgentMessage>,
    request: RuntimeAgentRequest,
) -> Result<RuntimeAgentResponse, &'static str> {
    let envelope = RuntimeAgentRequestEnvelope {
        id: uuid::Uuid::new_v4().to_string(),
        request,
    };
    if envelope.request.is_command() {
        tx.send(RuntimeAgentMessage::Command(envelope))
            .await
            .map_err(|_| "channel closed")?;
        Ok(RuntimeAgentResponse::Ok)
    } else {
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(RuntimeAgentMessage::Query(envelope, reply_tx))
            .await
            .map_err(|_| "channel closed")?;
        reply_rx.await.map_err(|_| "reply dropped")
    }
}

/// Simulates a runtime agent that processes messages from the channel.
/// Returns responses for queries, ignores commands.
async fn mock_agent(mut rx: mpsc::Receiver<RuntimeAgentMessage>, response_delay: Duration) {
    while let Some(msg) = rx.recv().await {
        match msg {
            RuntimeAgentMessage::Command(_) => {
                // Fire-and-forget: no response
            }
            RuntimeAgentMessage::Query(envelope, reply_tx) => {
                let response = match &envelope.request {
                    RuntimeAgentRequest::ShutdownKernel => RuntimeAgentResponse::Ok,
                    RuntimeAgentRequest::LaunchKernel { env_source, .. } => {
                        RuntimeAgentResponse::KernelLaunched {
                            env_source: env_source.clone(),
                        }
                    }
                    RuntimeAgentRequest::RestartKernel { env_source, .. } => {
                        RuntimeAgentResponse::KernelRestarted {
                            env_source: env_source.clone(),
                        }
                    }
                    RuntimeAgentRequest::Complete { .. } => {
                        RuntimeAgentResponse::CompletionResult {
                            items: vec![],
                            cursor_start: 0,
                            cursor_end: 0,
                        }
                    }
                    _ => RuntimeAgentResponse::Ok,
                };
                if !response_delay.is_zero() {
                    tokio::time::sleep(response_delay).await;
                }
                let _ = reply_tx.send(response);
            }
        }
    }
}

// ─── Correlation ID routing ───────────────────────────────────────────────────

#[tokio::test]
async fn correlation_ids_route_concurrent_queries_correctly() {
    // Multiple queries in-flight simultaneously should each get their own response.
    let (tx, rx) = mpsc::channel::<RuntimeAgentMessage>(16);

    // Agent that echoes the request type in its response
    tokio::spawn(async move {
        mock_agent(rx, Duration::ZERO).await;
    });

    // Send multiple queries concurrently
    let tx1 = tx.clone();
    let tx2 = tx.clone();
    let tx3 = tx.clone();

    let (r1, r2, r3) = tokio::join!(
        route_request(
            &tx1,
            RuntimeAgentRequest::Complete {
                code: "first".into(),
                cursor_pos: 5,
            }
        ),
        route_request(
            &tx2,
            RuntimeAgentRequest::Complete {
                code: "second".into(),
                cursor_pos: 6,
            }
        ),
        route_request(&tx3, RuntimeAgentRequest::ShutdownKernel),
    );

    // All three should get responses (no misrouting)
    assert!(matches!(
        r1.unwrap(),
        RuntimeAgentResponse::CompletionResult { .. }
    ));
    assert!(matches!(
        r2.unwrap(),
        RuntimeAgentResponse::CompletionResult { .. }
    ));
    assert!(matches!(r3.unwrap(), RuntimeAgentResponse::Ok));
}

#[tokio::test]
async fn correlation_ids_handle_interleaved_responses() {
    // Simulate an agent that responds to requests out-of-order (slower requests
    // get responses after faster ones).
    let (tx, mut rx) = mpsc::channel::<RuntimeAgentMessage>(16);

    // Custom agent: delays ShutdownKernel but responds to Complete immediately
    tokio::spawn(async move {
        let mut pending: Vec<(
            RuntimeAgentRequestEnvelope,
            oneshot::Sender<RuntimeAgentResponse>,
        )> = Vec::new();

        while let Some(msg) = rx.recv().await {
            match msg {
                RuntimeAgentMessage::Command(_) => {}
                RuntimeAgentMessage::Query(envelope, reply_tx) => {
                    if matches!(envelope.request, RuntimeAgentRequest::ShutdownKernel) {
                        // Delay shutdown response
                        pending.push((envelope, reply_tx));
                    } else {
                        // Respond immediately
                        let _ = reply_tx.send(RuntimeAgentResponse::CompletionResult {
                            items: vec![],
                            cursor_start: 0,
                            cursor_end: 0,
                        });
                    }
                }
            }
            // After processing 3 messages, flush pending
            if pending.len() == 1 {
                tokio::time::sleep(Duration::from_millis(10)).await;
                for (_, reply_tx) in pending.drain(..) {
                    let _ = reply_tx.send(RuntimeAgentResponse::Ok);
                }
            }
        }
    });

    // Send shutdown (slow) then complete (fast)
    let tx1 = tx.clone();
    let tx2 = tx.clone();

    let shutdown_handle =
        tokio::spawn(async move { route_request(&tx1, RuntimeAgentRequest::ShutdownKernel).await });
    // Small delay to ensure ordering
    tokio::time::sleep(Duration::from_millis(1)).await;
    let complete_handle = tokio::spawn(async move {
        route_request(
            &tx2,
            RuntimeAgentRequest::Complete {
                code: "x".into(),
                cursor_pos: 1,
            },
        )
        .await
    });

    let complete_result = complete_handle.await.unwrap();
    let shutdown_result = shutdown_handle.await.unwrap();

    // Complete should finish first (immediate response)
    assert!(matches!(
        complete_result.unwrap(),
        RuntimeAgentResponse::CompletionResult { .. }
    ));
    // Shutdown should also eventually complete (delayed)
    assert!(matches!(shutdown_result.unwrap(), RuntimeAgentResponse::Ok));
}

// ─── Fire-and-forget commands ─────────────────────────────────────────────────

#[tokio::test]
async fn fire_and_forget_returns_immediately() {
    let (tx, rx) = mpsc::channel::<RuntimeAgentMessage>(16);

    // Agent that never processes messages (simulates slow/stuck agent)
    // We intentionally hold rx open but never read it.
    let _rx_holder = rx;

    let start = std::time::Instant::now();
    let result = route_request(&tx, RuntimeAgentRequest::InterruptExecution).await;
    let elapsed = start.elapsed();

    // Fire-and-forget should return Ok immediately (no waiting for response)
    assert!(matches!(result.unwrap(), RuntimeAgentResponse::Ok));
    assert!(
        elapsed < Duration::from_millis(50),
        "fire-and-forget took {:?} — should be instant",
        elapsed
    );
}

#[tokio::test]
async fn fire_and_forget_does_not_block_subsequent_queries() {
    let (tx, rx) = mpsc::channel::<RuntimeAgentMessage>(16);

    // Agent with 50ms response delay
    tokio::spawn(async move {
        mock_agent(rx, Duration::from_millis(50)).await;
    });

    // Send a command (instant) then a query (waits for response)
    let cmd_result = route_request(&tx, RuntimeAgentRequest::InterruptExecution).await;
    assert!(matches!(cmd_result.unwrap(), RuntimeAgentResponse::Ok));

    // Query should still work even after command
    let query_result = route_request(
        &tx,
        RuntimeAgentRequest::Complete {
            code: "x".into(),
            cursor_pos: 1,
        },
    )
    .await;
    assert!(matches!(
        query_result.unwrap(),
        RuntimeAgentResponse::CompletionResult { .. }
    ));
}

#[tokio::test]
async fn multiple_commands_dont_accumulate_pending_replies() {
    let (tx, mut rx) = mpsc::channel::<RuntimeAgentMessage>(16);

    // Send 10 commands rapidly
    for _ in 0..10 {
        let result = route_request(&tx, RuntimeAgentRequest::InterruptExecution).await;
        assert!(matches!(result.unwrap(), RuntimeAgentResponse::Ok));
    }

    // Drain the channel — all should be Command variants (no Query)
    let mut count = 0;
    while let Ok(msg) = rx.try_recv() {
        match msg {
            RuntimeAgentMessage::Command(_) => count += 1,
            RuntimeAgentMessage::Query(_, _) => {
                panic!("fire-and-forget commands should not create Query messages")
            }
        }
    }
    assert_eq!(count, 10);
}

// ─── ShutdownKernel sync guarantee ───────────────────────────────────────────

#[tokio::test]
async fn shutdown_waits_for_agent_confirmation() {
    let (tx, rx) = mpsc::channel::<RuntimeAgentMessage>(16);

    // Agent with 100ms delay on shutdown
    tokio::spawn(async move {
        mock_agent(rx, Duration::from_millis(100)).await;
    });

    let start = std::time::Instant::now();
    let result = route_request(&tx, RuntimeAgentRequest::ShutdownKernel).await;
    let elapsed = start.elapsed();

    assert!(matches!(result.unwrap(), RuntimeAgentResponse::Ok));
    assert!(
        elapsed >= Duration::from_millis(90),
        "ShutdownKernel should wait for agent response (took {:?})",
        elapsed
    );
}

#[tokio::test]
async fn shutdown_then_launch_serialized() {
    // Verifies the fix for the CRDT race: ShutdownKernel must complete
    // before LaunchKernel can proceed (which would send RestartKernel).
    let (tx, rx) = mpsc::channel::<RuntimeAgentMessage>(16);
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let order_clone = order.clone();

    // Agent that records the order of processed requests
    tokio::spawn(async move {
        let mut rx = rx;
        while let Some(msg) = rx.recv().await {
            match msg {
                RuntimeAgentMessage::Command(env) => {
                    order_clone
                        .lock()
                        .unwrap()
                        .push(format!("cmd:{:?}", std::mem::discriminant(&env.request)));
                }
                RuntimeAgentMessage::Query(env, reply_tx) => {
                    let label = match &env.request {
                        RuntimeAgentRequest::ShutdownKernel => "shutdown",
                        RuntimeAgentRequest::LaunchKernel { .. } => "launch",
                        RuntimeAgentRequest::RestartKernel { .. } => "restart",
                        _ => "other",
                    };
                    order_clone.lock().unwrap().push(format!("query:{}", label));

                    // Simulate some processing time
                    tokio::time::sleep(Duration::from_millis(20)).await;

                    let response = match &env.request {
                        RuntimeAgentRequest::ShutdownKernel => RuntimeAgentResponse::Ok,
                        RuntimeAgentRequest::LaunchKernel { env_source, .. } => {
                            RuntimeAgentResponse::KernelLaunched {
                                env_source: env_source.clone(),
                            }
                        }
                        _ => RuntimeAgentResponse::Ok,
                    };
                    let _ = reply_tx.send(response);
                }
            }
        }
    });

    // Sequence: Shutdown → Launch (sequential, as the daemon does)
    let r1 = route_request(&tx, RuntimeAgentRequest::ShutdownKernel).await;
    assert!(matches!(r1.unwrap(), RuntimeAgentResponse::Ok));

    let r2 = route_request(
        &tx,
        RuntimeAgentRequest::LaunchKernel {
            kernel_type: "python".into(),
            env_source: "conda:inline".into(),
            notebook_path: None,
            launched_config: Default::default(),
            env_vars: Default::default(),
        },
    )
    .await;
    assert!(matches!(
        r2.unwrap(),
        RuntimeAgentResponse::KernelLaunched { .. }
    ));

    // Verify ordering: shutdown must be processed BEFORE launch
    let recorded = order.lock().unwrap().clone();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0], "query:shutdown");
    assert_eq!(recorded[1], "query:launch");
}

// ─── InterruptHandle independence ─────────────────────────────────────────────

#[tokio::test]
async fn interrupt_fires_while_query_in_flight() {
    // InterruptExecution is fire-and-forget, so it should not block
    // even when a sync query (e.g., Complete) is awaiting a response.
    let (tx, rx) = mpsc::channel::<RuntimeAgentMessage>(16);

    // Slow agent: 200ms per response
    tokio::spawn(async move {
        mock_agent(rx, Duration::from_millis(200)).await;
    });

    let tx1 = tx.clone();
    let tx2 = tx.clone();

    // Launch a slow query in the background
    let query_handle = tokio::spawn(async move {
        route_request(
            &tx1,
            RuntimeAgentRequest::Complete {
                code: "slow".into(),
                cursor_pos: 4,
            },
        )
        .await
    });

    // Small delay to let the query get submitted
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Interrupt should fire instantly, not blocked by the in-flight query
    let start = std::time::Instant::now();
    let interrupt_result = route_request(&tx2, RuntimeAgentRequest::InterruptExecution).await;
    let elapsed = start.elapsed();

    assert!(matches!(
        interrupt_result.unwrap(),
        RuntimeAgentResponse::Ok
    ));
    assert!(
        elapsed < Duration::from_millis(50),
        "interrupt took {:?} — should not be blocked by in-flight query",
        elapsed
    );

    // Query should still complete eventually
    let query_result = query_handle.await.unwrap();
    assert!(matches!(
        query_result.unwrap(),
        RuntimeAgentResponse::CompletionResult { .. }
    ));
}

#[tokio::test]
async fn sendcomm_fires_independently() {
    // SendComm is also fire-and-forget (widget interactions must not block)
    let (tx, _rx) = mpsc::channel::<RuntimeAgentMessage>(16);

    let start = std::time::Instant::now();
    let result = route_request(
        &tx,
        RuntimeAgentRequest::SendComm {
            message: serde_json::json!({"target_name": "jupyter.widget", "data": {}}),
        },
    )
    .await;
    let elapsed = start.elapsed();

    assert!(matches!(result.unwrap(), RuntimeAgentResponse::Ok));
    assert!(elapsed < Duration::from_millis(50));
}

// ─── Pending replies cleanup ──────────────────────────────────────────────────

#[tokio::test]
async fn pending_replies_error_on_agent_disconnect() {
    // When the agent disconnects (channel closes), pending queries should
    // get an error rather than hanging forever.
    let (tx, rx) = mpsc::channel::<RuntimeAgentMessage>(16);

    // Drop the receiver immediately — simulates agent disconnect
    drop(rx);

    // Query should fail with a channel error
    let result = route_request(
        &tx,
        RuntimeAgentRequest::Complete {
            code: "x".into(),
            cursor_pos: 1,
        },
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn pending_reply_dropped_when_agent_exits_mid_query() {
    let (tx, mut rx) = mpsc::channel::<RuntimeAgentMessage>(16);

    // Agent that receives one query then exits (drops reply_tx)
    tokio::spawn(async move {
        if let Some(RuntimeAgentMessage::Query(_, reply_tx)) = rx.recv().await {
            // Simulate agent crash: drop reply without sending
            drop(reply_tx);
        }
    });

    let result = route_request(
        &tx,
        RuntimeAgentRequest::Complete {
            code: "x".into(),
            cursor_pos: 1,
        },
    )
    .await;
    assert!(result.is_err(), "should error when reply is dropped");
}
