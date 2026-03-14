//! Broadcast receiver wrapper for kernel/execution events.
//!
//! Wraps `tokio::sync::broadcast::Receiver<NotebookBroadcast>` with a
//! simpler `Option`-based `recv()` API that handles `Lagged` errors by
//! logging and continuing, matching the behavior of the old
//! `NotebookBroadcastReceiver` in `notebook_sync_client.rs`.

use notebook_protocol::protocol::NotebookBroadcast;
use tokio::sync::broadcast;

/// A receiver for kernel/execution broadcasts from the daemon.
///
/// Wraps `tokio::sync::broadcast::Receiver` with a simpler API:
/// - `recv()` returns `Option<NotebookBroadcast>` (None = channel closed)
/// - Lagged messages are silently skipped with a warning log
/// - `resubscribe()` creates a new independent receiver
pub struct BroadcastReceiver {
    rx: broadcast::Receiver<NotebookBroadcast>,
}

impl BroadcastReceiver {
    /// Create a new receiver wrapping a raw broadcast receiver.
    pub fn new(rx: broadcast::Receiver<NotebookBroadcast>) -> Self {
        Self { rx }
    }

    /// Wait for the next broadcast event.
    ///
    /// Returns `None` if the channel is closed (all senders dropped).
    /// If events were missed due to a slow consumer, they are skipped
    /// and the next available event is returned.
    pub async fn recv(&mut self) -> Option<NotebookBroadcast> {
        loop {
            match self.rx.recv().await {
                Ok(msg) => return Some(msg),
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    log::warn!(
                        "[BroadcastReceiver] Lagged by {} messages, continuing",
                        count
                    );
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// Create a new receiver that will receive all future broadcasts.
    ///
    /// The new receiver starts from the next message sent after this call.
    /// Useful for creating independent receivers for streaming execution
    /// or subscription patterns.
    pub fn resubscribe(&self) -> Self {
        Self {
            rx: self.rx.resubscribe(),
        }
    }

    /// Get the raw broadcast receiver (for advanced use).
    pub fn into_inner(self) -> broadcast::Receiver<NotebookBroadcast> {
        self.rx
    }
}

impl From<broadcast::Receiver<NotebookBroadcast>> for BroadcastReceiver {
    fn from(rx: broadcast::Receiver<NotebookBroadcast>) -> Self {
        Self::new(rx)
    }
}
