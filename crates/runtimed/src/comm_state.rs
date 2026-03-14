//! Comm channel state tracking for widget synchronization.
//!
//! This module manages the state of active Jupyter comm channels (used by ipywidgets).
//! When a new client connects to a notebook room, the stored comm state is sent to allow
//! the client to reconstruct widget models that were created before it connected.
//!
//! ## Comm Protocol Overview
//!
//! The Jupyter comm protocol uses three message types:
//! - `comm_open`: Creates a new comm channel with initial state
//! - `comm_msg`: Sends updates or custom messages on the channel
//! - `comm_close`: Closes the channel
//!
//! For widgets, the `comm_open` establishes the model with initial state, and
//! `comm_msg` with `method: "update"` sends state deltas.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::RwLock;

/// A snapshot of a comm channel's state.
///
/// Re-export CommSnapshot from the shared protocol crate.
/// The canonical definition lives in `notebook_protocol::protocol`.
pub use notebook_protocol::protocol::CommSnapshot;

/// Internal entry with sequence number for ordering.
struct CommEntry {
    snapshot: CommSnapshot,
    seq: u64,
}

/// Thread-safe storage for active comm channels.
///
/// Tracks all active comm channels in a notebook room, allowing new clients
/// to receive the current state and reconstruct widget models.
///
/// Comms are returned in insertion order to ensure deterministic replay,
/// which matters for widgets that reference other widgets (e.g., layouts).
///
/// Also tracks Output widget capture contexts: when an Output widget sets
/// its `msg_id` field, outputs with matching `parent_header.msg_id` should
/// be routed to that widget instead of the cell outputs.
///
/// Supports nested Output widgets: multiple widgets can capture the same msg_id,
/// forming a stack. The most recently activated widget receives outputs, and
/// when it exits capture mode, the previous widget resumes capturing.
pub struct CommState {
    /// Active comms: comm_id -> (CommSnapshot, sequence_number)
    comms: RwLock<HashMap<String, CommEntry>>,
    /// Counter for insertion order
    next_seq: AtomicU64,
    /// Output widget capture contexts: capture_msg_id -> stack of widget_comm_ids
    /// When an Output widget sets state.msg_id, we push it onto the stack.
    /// The top of stack (last element) receives outputs.
    capture_contexts: RwLock<HashMap<String, Vec<String>>>,
    /// Reverse mapping: widget_comm_id -> capture_msg_id
    /// Used for efficient removal when a widget changes or clears its msg_id.
    widget_captures: RwLock<HashMap<String, String>>,
}

impl CommState {
    /// Create a new empty comm state.
    pub fn new() -> Self {
        Self {
            comms: RwLock::new(HashMap::new()),
            next_seq: AtomicU64::new(0),
            capture_contexts: RwLock::new(HashMap::new()),
            widget_captures: RwLock::new(HashMap::new()),
        }
    }

    /// Check if a comm entry is an Output widget by model name.
    fn is_output_widget(entry: &CommEntry) -> bool {
        entry.snapshot.model_name.as_deref() == Some("OutputModel")
    }

    /// Handle msg_id updates on Output widgets - manages capture context.
    ///
    /// When an Output widget sets `state.msg_id` to a non-empty value,
    /// it's entering capture mode. Outputs with `parent_header.msg_id`
    /// matching that value should be routed to the widget.
    ///
    /// Supports nested widgets: if multiple widgets capture the same msg_id,
    /// they form a stack. The most recently activated widget receives outputs.
    async fn on_output_widget_msg_id_change(&self, comm_id: &str, new_msg_id: &str) {
        let mut contexts = self.capture_contexts.write().await;
        let mut widget_caps = self.widget_captures.write().await;

        // Remove any existing capture for this widget (from previous msg_id)
        if let Some(old_msg_id) = widget_caps.remove(comm_id) {
            if let Some(stack) = contexts.get_mut(&old_msg_id) {
                stack.retain(|id| id != comm_id);
                // Clean up empty stacks
                if stack.is_empty() {
                    contexts.remove(&old_msg_id);
                }
            }
        }

        // If new_msg_id is non-empty, start capturing (push onto stack)
        if !new_msg_id.is_empty() {
            contexts
                .entry(new_msg_id.to_string())
                .or_default()
                .push(comm_id.to_string());
            widget_caps.insert(comm_id.to_string(), new_msg_id.to_string());
        }
    }

    /// Get widget comm_id if this msg_id is being captured by an Output widget.
    ///
    /// Returns Some(comm_id) if outputs with this parent_header.msg_id should
    /// be routed to an Output widget instead of cell outputs.
    ///
    /// For nested widgets, returns the most recently activated widget (top of stack).
    pub async fn get_capture_widget(&self, msg_id: &str) -> Option<String> {
        let contexts = self.capture_contexts.read().await;
        contexts.get(msg_id).and_then(|stack| stack.last().cloned())
    }

    /// Clear capture context when a widget closes.
    ///
    /// Uses reverse mapping for efficient removal without scanning all stacks.
    async fn clear_capture_for_widget(&self, comm_id: &str) {
        let mut contexts = self.capture_contexts.write().await;
        let mut widget_caps = self.widget_captures.write().await;

        // Use reverse map to find which msg_id this widget was capturing
        if let Some(msg_id) = widget_caps.remove(comm_id) {
            if let Some(stack) = contexts.get_mut(&msg_id) {
                stack.retain(|id| id != comm_id);
                // Clean up empty stacks
                if stack.is_empty() {
                    contexts.remove(&msg_id);
                }
            }
        }
    }

    /// Handle a `comm_open` message: create new comm entry.
    ///
    /// Extracts model info from the data payload and stores the initial state.
    pub async fn on_comm_open(
        &self,
        comm_id: &str,
        target_name: &str,
        data: &serde_json::Value,
        buffers: Vec<Vec<u8>>,
    ) {
        // Extract state from the data object (ipywidgets puts state in data.state)
        let state = data
            .get("state")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));

        // Extract model info from state for convenience
        let model_module = state
            .get("_model_module")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let model_name = state
            .get("_model_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let snapshot = CommSnapshot {
            comm_id: comm_id.to_string(),
            target_name: target_name.to_string(),
            state,
            model_module,
            model_name,
            buffers,
        };

        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let mut comms = self.comms.write().await;
        comms.insert(comm_id.to_string(), CommEntry { snapshot, seq });
    }

    /// Handle a `comm_msg` with `method: "update"`: merge state delta.
    ///
    /// Updates only the keys present in the delta, preserving other state.
    /// For Output widgets, also tracks msg_id changes for capture context.
    pub async fn on_comm_update(&self, comm_id: &str, state_delta: &serde_json::Value) {
        // First check if this is an Output widget with msg_id change
        let msg_id_change = {
            let comms = self.comms.read().await;
            if let Some(entry) = comms.get(comm_id) {
                if Self::is_output_widget(entry) {
                    state_delta
                        .get("msg_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                } else {
                    None
                }
            } else {
                None
            }
        };

        // Handle capture context change if needed (before holding comms lock)
        if let Some(new_msg_id) = msg_id_change {
            self.on_output_widget_msg_id_change(comm_id, &new_msg_id)
                .await;
        }

        // Now merge the state delta
        let mut comms = self.comms.write().await;
        if let Some(entry) = comms.get_mut(comm_id) {
            // Merge delta into existing state
            if let (Some(existing), Some(delta)) = (
                entry.snapshot.state.as_object_mut(),
                state_delta.as_object(),
            ) {
                for (key, value) in delta {
                    existing.insert(key.clone(), value.clone());
                }
            }
        }
        // If comm doesn't exist, ignore the update (might be out-of-order)
    }

    /// Handle a `comm_close` message: remove comm entry.
    ///
    /// Also clears any capture context associated with this widget.
    pub async fn on_comm_close(&self, comm_id: &str) {
        self.clear_capture_for_widget(comm_id).await;
        let mut comms = self.comms.write().await;
        comms.remove(comm_id);
    }

    /// Get all active comm snapshots in insertion order.
    ///
    /// Used to send current state to newly connected clients.
    /// Returns comms sorted by creation order, which ensures widget dependencies
    /// (e.g., layout models referenced by other widgets) are replayed correctly.
    pub async fn get_all(&self) -> Vec<CommSnapshot> {
        let comms = self.comms.read().await;
        let mut entries: Vec<_> = comms.values().collect();
        entries.sort_by_key(|e| e.seq);
        entries.into_iter().map(|e| e.snapshot.clone()).collect()
    }

    /// Clear all comm state.
    ///
    /// Called when the kernel shuts down, as all widgets become invalid.
    /// Also clears all capture contexts and reverse mappings.
    pub async fn clear(&self) {
        let mut comms = self.comms.write().await;
        comms.clear();
        self.next_seq.store(0, Ordering::Relaxed);

        let mut contexts = self.capture_contexts.write().await;
        contexts.clear();

        let mut widget_caps = self.widget_captures.write().await;
        widget_caps.clear();
    }

    /// Check if there are any active comms.
    pub async fn is_empty(&self) -> bool {
        let comms = self.comms.read().await;
        comms.is_empty()
    }
}

impl Default for CommState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_comm_open_creates_entry() {
        let state = CommState::new();
        let widget_state = serde_json::json!({
            "state": {
                "_model_name": "IntSliderModel",
                "_model_module": "@jupyter-widgets/controls",
                "value": 50,
                "min": 0,
                "max": 100
            }
        });

        state
            .on_comm_open("comm-1", "jupyter.widget", &widget_state, vec![])
            .await;

        let comms = state.get_all().await;
        assert_eq!(comms.len(), 1);
        assert_eq!(comms[0].comm_id, "comm-1");
        assert_eq!(comms[0].target_name, "jupyter.widget");
        assert_eq!(comms[0].model_name, Some("IntSliderModel".to_string()));
        assert_eq!(
            comms[0].model_module,
            Some("@jupyter-widgets/controls".to_string())
        );
        assert_eq!(comms[0].state["value"], 50);
    }

    #[tokio::test]
    async fn test_comm_update_merges_state() {
        let state = CommState::new();
        state
            .on_comm_open(
                "comm-1",
                "jupyter.widget",
                &serde_json::json!({
                    "state": {"value": 0, "min": 0, "max": 100}
                }),
                vec![],
            )
            .await;

        state
            .on_comm_update("comm-1", &serde_json::json!({"value": 50}))
            .await;

        let comms = state.get_all().await;
        assert_eq!(comms[0].state["value"], 50);
        assert_eq!(comms[0].state["min"], 0); // preserved
        assert_eq!(comms[0].state["max"], 100); // preserved
    }

    #[tokio::test]
    async fn test_comm_close_removes_entry() {
        let state = CommState::new();
        state
            .on_comm_open(
                "comm-1",
                "jupyter.widget",
                &serde_json::json!({"state": {}}),
                vec![],
            )
            .await;

        assert!(!state.is_empty().await);
        state.on_comm_close("comm-1").await;
        assert!(state.is_empty().await);
    }

    #[tokio::test]
    async fn test_clear_removes_all_entries() {
        let state = CommState::new();
        state
            .on_comm_open(
                "comm-1",
                "jupyter.widget",
                &serde_json::json!({"state": {}}),
                vec![],
            )
            .await;
        state
            .on_comm_open(
                "comm-2",
                "jupyter.widget",
                &serde_json::json!({"state": {}}),
                vec![],
            )
            .await;

        assert_eq!(state.get_all().await.len(), 2);
        state.clear().await;
        assert!(state.is_empty().await);
    }

    #[tokio::test]
    async fn test_update_nonexistent_comm_is_ignored() {
        let state = CommState::new();

        // Update for a comm that doesn't exist should not panic
        state
            .on_comm_update("nonexistent", &serde_json::json!({"value": 42}))
            .await;

        assert!(state.is_empty().await);
    }

    #[tokio::test]
    async fn test_buffers_are_stored() {
        let state = CommState::new();
        let buffers = vec![vec![1, 2, 3], vec![4, 5, 6]];

        state
            .on_comm_open(
                "comm-1",
                "jupyter.widget",
                &serde_json::json!({"state": {}}),
                buffers.clone(),
            )
            .await;

        let comms = state.get_all().await;
        assert_eq!(comms[0].buffers, buffers);
    }

    #[tokio::test]
    async fn test_get_all_returns_insertion_order() {
        let state = CommState::new();

        // Insert comms in a specific order
        for i in 0..10 {
            state
                .on_comm_open(
                    &format!("comm-{}", i),
                    "jupyter.widget",
                    &serde_json::json!({"state": {"index": i}}),
                    vec![],
                )
                .await;
        }

        // Verify they come back in insertion order
        let comms = state.get_all().await;
        assert_eq!(comms.len(), 10);
        for (i, comm) in comms.iter().enumerate() {
            assert_eq!(comm.comm_id, format!("comm-{}", i));
        }
    }

    #[tokio::test]
    async fn test_output_widget_capture_context() {
        let state = CommState::new();

        // Create an Output widget
        state
            .on_comm_open(
                "output-widget-1",
                "jupyter.widget",
                &serde_json::json!({
                    "state": {
                        "_model_name": "OutputModel",
                        "_model_module": "@jupyter-widgets/output",
                        "msg_id": "",
                        "outputs": []
                    }
                }),
                vec![],
            )
            .await;

        // Initially no capture
        assert!(state.get_capture_widget("some-msg-id").await.is_none());

        // Set msg_id to start capturing
        state
            .on_comm_update(
                "output-widget-1",
                &serde_json::json!({"msg_id": "exec-123"}),
            )
            .await;

        // Now should capture
        assert_eq!(
            state.get_capture_widget("exec-123").await,
            Some("output-widget-1".to_string())
        );

        // Different msg_id should not match
        assert!(state.get_capture_widget("exec-456").await.is_none());

        // Clear msg_id to stop capturing
        state
            .on_comm_update("output-widget-1", &serde_json::json!({"msg_id": ""}))
            .await;

        // No longer capturing
        assert!(state.get_capture_widget("exec-123").await.is_none());
    }

    #[tokio::test]
    async fn test_output_widget_close_clears_capture() {
        let state = CommState::new();

        // Create and activate an Output widget
        state
            .on_comm_open(
                "output-widget-1",
                "jupyter.widget",
                &serde_json::json!({
                    "state": {
                        "_model_name": "OutputModel",
                        "msg_id": ""
                    }
                }),
                vec![],
            )
            .await;

        state
            .on_comm_update(
                "output-widget-1",
                &serde_json::json!({"msg_id": "exec-123"}),
            )
            .await;

        assert!(state.get_capture_widget("exec-123").await.is_some());

        // Close the widget
        state.on_comm_close("output-widget-1").await;

        // Capture should be cleared
        assert!(state.get_capture_widget("exec-123").await.is_none());
    }

    #[tokio::test]
    async fn test_non_output_widget_msg_id_ignored() {
        let state = CommState::new();

        // Create a non-Output widget with a msg_id field
        state
            .on_comm_open(
                "slider-1",
                "jupyter.widget",
                &serde_json::json!({
                    "state": {
                        "_model_name": "IntSliderModel",
                        "msg_id": ""
                    }
                }),
                vec![],
            )
            .await;

        // Update msg_id on non-Output widget
        state
            .on_comm_update("slider-1", &serde_json::json!({"msg_id": "exec-123"}))
            .await;

        // Should NOT create a capture context
        assert!(state.get_capture_widget("exec-123").await.is_none());
    }

    #[tokio::test]
    async fn test_nested_output_widgets_capture() {
        let state = CommState::new();

        // Create two Output widgets (simulating nested `with out:` blocks)
        state
            .on_comm_open(
                "outer-widget",
                "jupyter.widget",
                &serde_json::json!({
                    "state": {
                        "_model_name": "OutputModel",
                        "msg_id": ""
                    }
                }),
                vec![],
            )
            .await;

        state
            .on_comm_open(
                "inner-widget",
                "jupyter.widget",
                &serde_json::json!({
                    "state": {
                        "_model_name": "OutputModel",
                        "msg_id": ""
                    }
                }),
                vec![],
            )
            .await;

        // Outer widget starts capturing
        state
            .on_comm_update("outer-widget", &serde_json::json!({"msg_id": "exec-123"}))
            .await;

        assert_eq!(
            state.get_capture_widget("exec-123").await,
            Some("outer-widget".to_string())
        );

        // Inner widget starts capturing same msg_id (nested context)
        state
            .on_comm_update("inner-widget", &serde_json::json!({"msg_id": "exec-123"}))
            .await;

        // Inner widget (top of stack) should now receive outputs
        assert_eq!(
            state.get_capture_widget("exec-123").await,
            Some("inner-widget".to_string())
        );

        // Inner widget exits capture mode
        state
            .on_comm_update("inner-widget", &serde_json::json!({"msg_id": ""}))
            .await;

        // Outer widget resumes capturing (was underneath on stack)
        assert_eq!(
            state.get_capture_widget("exec-123").await,
            Some("outer-widget".to_string())
        );

        // Outer widget exits capture mode
        state
            .on_comm_update("outer-widget", &serde_json::json!({"msg_id": ""}))
            .await;

        // No more captures
        assert!(state.get_capture_widget("exec-123").await.is_none());
    }

    #[tokio::test]
    async fn test_widget_switching_capture_msg_id() {
        let state = CommState::new();

        // Create an Output widget
        state
            .on_comm_open(
                "output-1",
                "jupyter.widget",
                &serde_json::json!({
                    "state": {
                        "_model_name": "OutputModel",
                        "msg_id": ""
                    }
                }),
                vec![],
            )
            .await;

        // Start capturing msg_id A
        state
            .on_comm_update("output-1", &serde_json::json!({"msg_id": "exec-A"}))
            .await;

        assert_eq!(
            state.get_capture_widget("exec-A").await,
            Some("output-1".to_string())
        );

        // Switch to capturing msg_id B (widget can only capture one at a time)
        state
            .on_comm_update("output-1", &serde_json::json!({"msg_id": "exec-B"}))
            .await;

        // Should no longer capture A
        assert!(state.get_capture_widget("exec-A").await.is_none());
        // Should now capture B
        assert_eq!(
            state.get_capture_widget("exec-B").await,
            Some("output-1".to_string())
        );
    }
}
