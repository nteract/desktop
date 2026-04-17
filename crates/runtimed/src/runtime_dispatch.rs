//! Runtime dispatch enum — routes `KernelConnection` calls to the
//! appropriate backend (Jupyter ZMQ kernel or in-memory test runtime).

use std::path::PathBuf;

use anyhow::Result;
use notebook_protocol::protocol::LaunchedEnvConfig;

use crate::jupyter_kernel::JupyterKernel;
use crate::kernel_connection::KernelConnection;
use crate::kernel_manager::QueueCommand;
use crate::protocol::{CompletionItem, HistoryEntry};
use crate::test_runtime::TestRuntime;

pub enum Runtime {
    Jupyter(Box<JupyterKernel>),
    Test(Box<TestRuntime>),
}

macro_rules! dispatch {
    ($self:ident, $method:ident $(, $arg:expr)*) => {
        match $self {
            Runtime::Jupyter(k) => k.$method($($arg),*).await,
            Runtime::Test(k) => k.$method($($arg),*).await,
        }
    };
}

impl KernelConnection for Runtime {
    async fn launch(
        _config: crate::kernel_connection::KernelLaunchConfig,
        _shared: crate::kernel_connection::KernelSharedRefs,
    ) -> Result<(Self, tokio::sync::mpsc::Receiver<QueueCommand>)> {
        unreachable!("callers construct Runtime::Jupyter or Runtime::Test directly")
    }

    async fn execute(&mut self, cell_id: &str, execution_id: &str, source: &str) -> Result<()> {
        dispatch!(self, execute, cell_id, execution_id, source)
    }

    async fn interrupt(&mut self) -> Result<()> {
        dispatch!(self, interrupt)
    }

    async fn shutdown(&mut self) -> Result<()> {
        dispatch!(self, shutdown)
    }

    async fn send_comm_message(&mut self, raw_message: serde_json::Value) -> Result<()> {
        dispatch!(self, send_comm_message, raw_message)
    }

    async fn send_comm_update(&mut self, comm_id: &str, state: serde_json::Value) -> Result<()> {
        dispatch!(self, send_comm_update, comm_id, state)
    }

    async fn complete(
        &mut self,
        code: &str,
        cursor_pos: usize,
    ) -> Result<(Vec<CompletionItem>, usize, usize)> {
        dispatch!(self, complete, code, cursor_pos)
    }

    async fn get_history(
        &mut self,
        pattern: Option<&str>,
        n: i32,
        unique: bool,
    ) -> Result<Vec<HistoryEntry>> {
        dispatch!(self, get_history, pattern, n, unique)
    }

    fn kernel_type(&self) -> &str {
        match self {
            Runtime::Jupyter(k) => k.kernel_type(),
            Runtime::Test(k) => k.kernel_type(),
        }
    }

    fn env_source(&self) -> &str {
        match self {
            Runtime::Jupyter(k) => k.env_source(),
            Runtime::Test(k) => k.env_source(),
        }
    }

    fn launched_config(&self) -> &LaunchedEnvConfig {
        match self {
            Runtime::Jupyter(k) => k.launched_config(),
            Runtime::Test(k) => k.launched_config(),
        }
    }

    fn env_path(&self) -> Option<&PathBuf> {
        match self {
            Runtime::Jupyter(k) => k.env_path(),
            Runtime::Test(k) => k.env_path(),
        }
    }

    fn is_connected(&self) -> bool {
        match self {
            Runtime::Jupyter(k) => k.is_connected(),
            Runtime::Test(k) => k.is_connected(),
        }
    }

    fn update_launched_uv_deps(&mut self, deps: Vec<String>) {
        match self {
            Runtime::Jupyter(k) => k.update_launched_uv_deps(deps),
            Runtime::Test(k) => k.update_launched_uv_deps(deps),
        }
    }
}
