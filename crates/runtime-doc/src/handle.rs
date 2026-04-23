use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

use crate::{RuntimeStateDoc, RuntimeStateError};

/// Handle to a per-notebook RuntimeStateDoc.
///
/// All mutations go through `with_doc` (sync) or `fork`/`merge` (async).
/// Notification is automatic via heads comparison. Clone is cheap.
///
/// Uses `std::sync::Mutex`, not `tokio::sync::RwLock`. Automerge writes are
/// microsecond-fast. The `!Send` guard prevents holding across `.await`.
#[derive(Clone)]
pub struct RuntimeStateHandle {
    doc: Arc<Mutex<RuntimeStateDoc>>,
    changed_tx: broadcast::Sender<()>,
}

impl RuntimeStateHandle {
    pub fn new(doc: RuntimeStateDoc, changed_tx: broadcast::Sender<()>) -> Self {
        Self {
            doc: Arc::new(Mutex::new(doc)),
            changed_tx,
        }
    }

    /// Synchronous mutation. Acquires mutex, runs closure, notifies if heads changed.
    ///
    /// Notification fires even if the closure returns `Err`, because earlier
    /// mutations in a batched closure may have already changed the doc before
    /// a later write failed.
    pub fn with_doc<F, T>(&self, f: F) -> Result<T, RuntimeStateError>
    where
        F: FnOnce(&mut RuntimeStateDoc) -> Result<T, RuntimeStateError>,
    {
        let mut sd = self
            .doc
            .lock()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        let heads_before = sd.get_heads();
        let result = f(&mut sd);
        if sd.get_heads() != heads_before {
            let _ = self.changed_tx.send(());
        }
        result
    }

    /// Fork at current heads for async work. Never uses fork_at (automerge#1327).
    pub fn fork(&self, actor_label: &str) -> Result<RuntimeStateDoc, RuntimeStateError> {
        let mut sd = self
            .doc
            .lock()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        Ok(sd.fork_with_actor(actor_label))
    }

    /// Merge a fork back. Notifies if heads changed.
    ///
    /// If merge panics (Automerge's apply path is not transactional),
    /// catches the unwind and rebuilds the doc via save/load to restore
    /// a consistent state. The fork's writes are lost but the session
    /// continues.
    pub fn merge(&self, fork: &mut RuntimeStateDoc) -> Result<(), RuntimeStateError> {
        let mut sd = self
            .doc
            .lock()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        let heads_before = sd.get_heads();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sd.merge(fork))) {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                // Normal error: doc is unchanged (error fires before mutation).
                return Err(e.into());
            }
            Err(_panic) => {
                // Panic during apply: doc may be half-merged. Rebuild from save.
                tracing::warn!(
                    "[runtime-state] merge panicked, rebuilding from save to restore consistency"
                );
                sd.rebuild_from_save();
                return Err(RuntimeStateError::MissingScaffold(
                    "merge panicked, rebuilt from save",
                ));
            }
        }
        if sd.get_heads() != heads_before {
            let _ = self.changed_tx.send(());
        }
        Ok(())
    }

    /// Read-only access. No notification.
    pub fn read<F, T>(&self, f: F) -> Result<T, RuntimeStateError>
    where
        F: FnOnce(&RuntimeStateDoc) -> T,
    {
        let sd = self
            .doc
            .lock()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        Ok(f(&sd))
    }

    /// Subscribe to change notifications.
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.changed_tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_handle() -> RuntimeStateHandle {
        let doc = RuntimeStateDoc::new();
        let (tx, _) = broadcast::channel(16);
        RuntimeStateHandle::new(doc, tx)
    }

    #[test]
    fn with_doc_notifies_on_change() {
        let handle = make_handle();
        let mut rx = handle.subscribe();
        handle.with_doc(|sd| sd.set_kernel_status("busy")).unwrap();
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn with_doc_skips_notification_when_unchanged() {
        let handle = make_handle();
        handle.with_doc(|sd| sd.set_kernel_status("busy")).unwrap();
        let mut rx = handle.subscribe();
        handle.with_doc(|sd| sd.set_kernel_status("busy")).unwrap();
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn batched_writes_single_notification() {
        let handle = make_handle();
        let mut rx = handle.subscribe();
        handle
            .with_doc(|sd| {
                sd.set_kernel_status("busy")?;
                sd.set_starting_phase("resolving")?;
                Ok(())
            })
            .unwrap();
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn fork_and_merge_notifies() {
        let handle = make_handle();
        let mut rx = handle.subscribe();
        let mut fork = handle.fork("test-fork").unwrap();
        fork.set_kernel_status("idle").unwrap();
        handle.merge(&mut fork).unwrap();
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn read_does_not_notify() {
        let handle = make_handle();
        handle.with_doc(|sd| sd.set_kernel_status("busy")).unwrap();
        let mut rx = handle.subscribe();
        let status = handle
            .read(|sd| sd.read_state().kernel.status.clone())
            .unwrap();
        assert_eq!(status, "busy");
        assert!(rx.try_recv().is_err());
    }
}
