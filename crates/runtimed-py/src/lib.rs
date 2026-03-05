//! Python bindings for runtimed daemon client.
//!
//! Provides Python classes for:
//! - `DaemonClient`: Low-level daemon operations (status, ping, list rooms)
//! - `Session`: Synchronous code execution with kernel management
//! - `AsyncSession`: Async code execution with kernel management
//! - `ExecutionEventStream`: Async iterator over execution events
//! - `ExecutionEventIterator`: Sync iterator over execution events
//!
//! Both sync and async APIs are provided with full feature parity.

use pyo3::prelude::*;

mod async_session;
mod client;
mod error;
mod event_stream;
mod output;
mod output_resolver;
mod session;
mod subscription;

use async_session::AsyncSession;
use client::DaemonClient;
use error::RuntimedError;
use event_stream::{ExecutionEventIterator, ExecutionEventStream};
use output::{Cell, ExecutionEvent, ExecutionResult, Output};
use session::Session;
use subscription::{EventIteratorSubscription, EventSubscription};

/// Python module for runtimed daemon client.
#[pymodule]
fn runtimed(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Core classes - sync API
    m.add_class::<DaemonClient>()?;
    m.add_class::<Session>()?;

    // Core classes - async API
    m.add_class::<AsyncSession>()?;

    // Iterator types for streaming execution
    m.add_class::<ExecutionEventStream>()?;
    m.add_class::<ExecutionEventIterator>()?;

    // Subscription types for independent event listening
    m.add_class::<EventSubscription>()?;
    m.add_class::<EventIteratorSubscription>()?;

    // Output types
    m.add_class::<Cell>()?;
    m.add_class::<ExecutionResult>()?;
    m.add_class::<ExecutionEvent>()?;
    m.add_class::<Output>()?;

    // Error type
    m.add("RuntimedError", m.py().get_type::<RuntimedError>())?;

    Ok(())
}
