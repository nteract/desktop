//! Explicit connection/bootstrap status for a synced notebook handle.

use notebook_protocol::protocol::{
    InitialLoadPhaseWire, NotebookDocPhaseWire, RuntimeStatePhaseWire, SessionSyncStatusWire,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Connected,
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NotebookDocPhase {
    Pending,
    Syncing,
    Interactive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RuntimeStatePhase {
    Pending,
    Syncing,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitialLoadPhase {
    NotNeeded,
    Streaming,
    Ready,
    Failed { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncStatus {
    pub connection: ConnectionState,
    pub notebook_doc: NotebookDocPhase,
    pub runtime_state: RuntimeStatePhase,
    pub initial_load: InitialLoadPhase,
}

impl SyncStatus {
    pub fn connected_pending() -> Self {
        Self {
            connection: ConnectionState::Connected,
            notebook_doc: NotebookDocPhase::Pending,
            runtime_state: RuntimeStatePhase::Pending,
            initial_load: InitialLoadPhase::NotNeeded,
        }
    }

    pub fn session_ready(&self) -> bool {
        self.notebook_doc == NotebookDocPhase::Interactive
            && self.runtime_state == RuntimeStatePhase::Ready
            && matches!(
                self.initial_load,
                InitialLoadPhase::NotNeeded | InitialLoadPhase::Ready
            )
    }
}

impl Default for SyncStatus {
    fn default() -> Self {
        Self::connected_pending()
    }
}

impl From<NotebookDocPhaseWire> for NotebookDocPhase {
    fn from(value: NotebookDocPhaseWire) -> Self {
        match value {
            NotebookDocPhaseWire::Pending => Self::Pending,
            NotebookDocPhaseWire::Syncing => Self::Syncing,
            NotebookDocPhaseWire::Interactive => Self::Interactive,
        }
    }
}

impl From<RuntimeStatePhaseWire> for RuntimeStatePhase {
    fn from(value: RuntimeStatePhaseWire) -> Self {
        match value {
            RuntimeStatePhaseWire::Pending => Self::Pending,
            RuntimeStatePhaseWire::Syncing => Self::Syncing,
            RuntimeStatePhaseWire::Ready => Self::Ready,
        }
    }
}

impl From<InitialLoadPhaseWire> for InitialLoadPhase {
    fn from(value: InitialLoadPhaseWire) -> Self {
        match value {
            InitialLoadPhaseWire::NotNeeded => Self::NotNeeded,
            InitialLoadPhaseWire::Streaming => Self::Streaming,
            InitialLoadPhaseWire::Ready => Self::Ready,
            InitialLoadPhaseWire::Failed { reason } => Self::Failed { reason },
        }
    }
}

impl From<SessionSyncStatusWire> for SyncStatus {
    fn from(value: SessionSyncStatusWire) -> Self {
        Self {
            connection: ConnectionState::Connected,
            notebook_doc: value.notebook_doc.into(),
            runtime_state: value.runtime_state.into(),
            initial_load: value.initial_load.into(),
        }
    }
}
