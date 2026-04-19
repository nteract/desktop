use std::any::Any;
use std::future::Future;
use std::panic::AssertUnwindSafe;

use futures::FutureExt;
use tokio::task::JoinHandle;

pub struct PanicInfo {
    pub label: &'static str,
    pub message: String,
}

pub fn panic_payload_to_string(payload: Box<dyn Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else {
        "unknown panic".to_string()
    }
}

pub fn spawn_supervised<F, P>(label: &'static str, fut: F, on_panic: P) -> JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
    P: FnOnce(&PanicInfo) + Send + 'static,
{
    tokio::spawn(async move {
        if let Err(payload) = AssertUnwindSafe(fut).catch_unwind().await {
            let message = panic_payload_to_string(payload);
            let info = PanicInfo { label, message };
            tracing::error!(
                "[task-supervisor] '{}' panicked: {}",
                info.label,
                info.message
            );
            // Defense-in-depth: on_panic must not itself panic.
            if let Err(callback_payload) =
                std::panic::catch_unwind(AssertUnwindSafe(|| on_panic(&info)))
            {
                let msg = panic_payload_to_string(callback_payload);
                tracing::error!(
                    "[task-supervisor] on_panic callback for '{}' panicked: {}",
                    label,
                    msg
                );
            }
        }
    })
}

pub fn spawn_best_effort<F>(label: &'static str, fut: F) -> JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    spawn_supervised(label, fut, |_| {})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_spawn_best_effort_normal() {
        let handle = spawn_best_effort("test-normal", async {});
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_spawn_best_effort_panic() {
        let handle = spawn_best_effort("test-panic", async {
            panic!("intentional test panic");
        });
        // Panic is caught internally — JoinHandle resolves Ok
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_spawn_supervised_calls_on_panic() {
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();

        let handle = spawn_supervised(
            "test-supervised",
            async { panic!("supervised panic") },
            move |info| {
                assert_eq!(info.label, "test-supervised");
                assert!(info.message.contains("supervised panic"));
                called_clone.store(true, Ordering::Relaxed);
            },
        );
        handle.await.unwrap();
        assert!(called.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_spawn_supervised_no_panic() {
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();

        let handle = spawn_supervised("test-no-panic", async {}, move |_| {
            called_clone.store(true, Ordering::Relaxed);
        });
        handle.await.unwrap();
        assert!(!called.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_spawn_supervised_abort() {
        let handle = spawn_supervised(
            "test-abort",
            async {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            },
            |_| {},
        );
        handle.abort();
        // Aborted task resolves to Err(JoinError::cancelled)
        assert!(handle.await.unwrap_err().is_cancelled());
    }
}
