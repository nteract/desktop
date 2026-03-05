//! Event subscription for independent event listening.
//!
//! Allows subscribing to notebook broadcasts independently of execution,
//! enabling reactive patterns for agents and UIs.

use pyo3::exceptions::PyStopAsyncIteration;
use pyo3::prelude::*;
use pyo3_async_runtimes::tokio::future_into_py;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use runtimed::notebook_sync_client::NotebookBroadcastReceiver;
use runtimed::protocol::NotebookBroadcast;

use crate::error::to_py_err;
use crate::output::ExecutionEvent;
use crate::output_resolver;

/// Async subscription to notebook broadcasts.
///
/// Yields all broadcast events from the notebook, optionally filtered by
/// cell IDs and event types. This enables reactive patterns for agents
/// that want to respond to any document activity.
///
/// Example:
///     ```python
///     async for event in session.subscribe():
///         print(f"Got event: {event.event_type}")
///
///     # With filters
///     async for event in session.subscribe(event_types=["output", "done"]):
///         if event.event_type == "output":
///             print(event.output.text)
///     ```
#[pyclass]
pub struct EventSubscription {
    state: Arc<Mutex<SubscriptionState>>,
}

struct SubscriptionState {
    broadcast_rx: NotebookBroadcastReceiver,
    /// Filter to specific cell IDs (empty = all cells)
    cell_ids: HashSet<String>,
    /// Filter to specific event types (empty = all types)
    event_types: HashSet<String>,
    /// For resolving blob outputs
    blob_base_url: Option<String>,
    blob_store_path: Option<PathBuf>,
    /// Whether subscription is closed
    closed: bool,
}

impl EventSubscription {
    pub fn new(
        broadcast_rx: NotebookBroadcastReceiver,
        cell_ids: Option<Vec<String>>,
        event_types: Option<Vec<String>>,
        blob_base_url: Option<String>,
        blob_store_path: Option<PathBuf>,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(SubscriptionState {
                broadcast_rx,
                cell_ids: cell_ids
                    .map(|v| v.into_iter().collect())
                    .unwrap_or_default(),
                event_types: event_types
                    .map(|v| v.into_iter().collect())
                    .unwrap_or_default(),
                blob_base_url,
                blob_store_path,
                closed: false,
            })),
        }
    }
}

#[pymethods]
impl EventSubscription {
    fn __aiter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);

        future_into_py(py, async move {
            let mut state = state.lock().await;

            if state.closed {
                return Err(PyStopAsyncIteration::new_err("Subscription closed"));
            }

            let cell_ids = state.cell_ids.clone();
            let event_types = state.event_types.clone();
            let blob_base_url = state.blob_base_url.clone();
            let blob_store_path = state.blob_store_path.clone();

            // Wait for next matching broadcast
            loop {
                match state.broadcast_rx.recv().await {
                    Some(broadcast) => {
                        if let Some(event) = broadcast_to_event(
                            broadcast,
                            &cell_ids,
                            &event_types,
                            &blob_base_url,
                            &blob_store_path,
                        )
                        .await
                        {
                            return Ok(event);
                        }
                        // Didn't match filters, continue waiting
                    }
                    None => {
                        state.closed = true;
                        return Err(PyStopAsyncIteration::new_err("Broadcast channel closed"));
                    }
                }
            }
        })
    }

    /// Close the subscription.
    fn close<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = Arc::clone(&self.state);
        future_into_py(py, async move {
            let mut state = state.lock().await;
            state.closed = true;
            Ok(())
        })
    }

    fn __repr__(&self) -> String {
        "EventSubscription(...)".to_string()
    }
}

/// Sync subscription to notebook broadcasts.
#[pyclass]
pub struct EventIteratorSubscription {
    runtime: tokio::runtime::Runtime,
    state: Arc<Mutex<SubscriptionState>>,
}

impl EventIteratorSubscription {
    pub fn new(
        broadcast_rx: NotebookBroadcastReceiver,
        cell_ids: Option<Vec<String>>,
        event_types: Option<Vec<String>>,
        blob_base_url: Option<String>,
        blob_store_path: Option<PathBuf>,
    ) -> PyResult<Self> {
        let runtime = tokio::runtime::Runtime::new().map_err(to_py_err)?;
        Ok(Self {
            runtime,
            state: Arc::new(Mutex::new(SubscriptionState {
                broadcast_rx,
                cell_ids: cell_ids
                    .map(|v| v.into_iter().collect())
                    .unwrap_or_default(),
                event_types: event_types
                    .map(|v| v.into_iter().collect())
                    .unwrap_or_default(),
                blob_base_url,
                blob_store_path,
                closed: false,
            })),
        })
    }
}

#[pymethods]
impl EventIteratorSubscription {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&self) -> PyResult<Option<ExecutionEvent>> {
        let state = Arc::clone(&self.state);

        self.runtime.block_on(async {
            let mut state = state.lock().await;

            if state.closed {
                return Ok(None);
            }

            let cell_ids = state.cell_ids.clone();
            let event_types = state.event_types.clone();
            let blob_base_url = state.blob_base_url.clone();
            let blob_store_path = state.blob_store_path.clone();

            loop {
                match state.broadcast_rx.recv().await {
                    Some(broadcast) => {
                        if let Some(event) = broadcast_to_event(
                            broadcast,
                            &cell_ids,
                            &event_types,
                            &blob_base_url,
                            &blob_store_path,
                        )
                        .await
                        {
                            return Ok(Some(event));
                        }
                    }
                    None => {
                        state.closed = true;
                        return Ok(None);
                    }
                }
            }
        })
    }

    /// Close the subscription.
    fn close(&self) -> PyResult<()> {
        self.runtime.block_on(async {
            let mut state = self.state.lock().await;
            state.closed = true;
            Ok(())
        })
    }

    fn __repr__(&self) -> String {
        "EventIteratorSubscription(...)".to_string()
    }
}

/// Convert a broadcast to an ExecutionEvent, applying filters.
/// Returns None if the broadcast doesn't match the filters.
async fn broadcast_to_event(
    broadcast: NotebookBroadcast,
    cell_ids: &HashSet<String>,
    event_types: &HashSet<String>,
    blob_base_url: &Option<String>,
    blob_store_path: &Option<PathBuf>,
) -> Option<ExecutionEvent> {
    match broadcast {
        NotebookBroadcast::ExecutionStarted {
            cell_id,
            execution_count,
        } => {
            if !cell_ids.is_empty() && !cell_ids.contains(&cell_id) {
                return None;
            }
            if !event_types.is_empty() && !event_types.contains("execution_started") {
                return None;
            }
            Some(ExecutionEvent::execution_started(&cell_id, execution_count))
        }
        NotebookBroadcast::Output {
            cell_id,
            output_type,
            output_json,
            output_index,
        } => {
            if !cell_ids.is_empty() && !cell_ids.contains(&cell_id) {
                return None;
            }
            if !event_types.is_empty() && !event_types.contains("output") {
                return None;
            }
            // Resolve and include the output
            if let Some(output) = output_resolver::resolve_output_with_type(
                &output_type,
                &output_json,
                blob_base_url,
                blob_store_path,
            )
            .await
            {
                Some(ExecutionEvent::output_with_index(
                    &cell_id,
                    output,
                    output_index,
                ))
            } else {
                // Still return an event with output_index even if resolution failed
                Some(ExecutionEvent::output_signal(&cell_id, output_index))
            }
        }
        NotebookBroadcast::ExecutionDone { cell_id } => {
            if !cell_ids.is_empty() && !cell_ids.contains(&cell_id) {
                return None;
            }
            if !event_types.is_empty() && !event_types.contains("done") {
                return None;
            }
            Some(ExecutionEvent::done(&cell_id))
        }
        NotebookBroadcast::KernelError { error } => {
            if !event_types.is_empty() && !event_types.contains("error") {
                return None;
            }
            Some(ExecutionEvent::error("", &error))
        }
        NotebookBroadcast::KernelStatus { status, cell_id } => {
            if !event_types.is_empty() && !event_types.contains("kernel_status") {
                return None;
            }
            // Create a special event for kernel status
            Some(ExecutionEvent {
                event_type: "kernel_status".to_string(),
                cell_id: cell_id.unwrap_or_default(),
                output: None,
                output_index: None,
                execution_count: None,
                error_message: Some(status),
            })
        }
        _ => {
            // Ignore other broadcast types (QueueChanged, Comm, etc.)
            None
        }
    }
}
