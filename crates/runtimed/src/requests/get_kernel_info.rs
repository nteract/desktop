//! Compatibility handler for `NotebookRequest::GetKernelInfo`.
//!
//! Current clients should read `RuntimeStateDoc` directly. This remains on the
//! wire so an older app UI can still talk to the newer daemon it launches
//! during upgrade.

use runtime_doc::RuntimeLifecycle;

use crate::notebook_sync_server::NotebookRoom;
use crate::protocol::NotebookResponse;

pub(crate) async fn handle(room: &NotebookRoom) -> NotebookResponse {
    let state = room.state.read(|sd| sd.read_state());
    match state {
        Ok(state) if !matches!(state.kernel.lifecycle, RuntimeLifecycle::NotStarted) => {
            let (legacy_status, _phase) = state.kernel.lifecycle.to_legacy();
            NotebookResponse::KernelInfo {
                kernel_type: if state.kernel.name.is_empty() {
                    None
                } else {
                    Some(state.kernel.name)
                },
                env_source: if state.kernel.env_source.is_empty() {
                    None
                } else {
                    Some(state.kernel.env_source)
                },
                status: legacy_status.to_string(),
            }
        }
        _ => NotebookResponse::KernelInfo {
            kernel_type: None,
            env_source: None,
            status: "not_started".to_string(),
        },
    }
}
