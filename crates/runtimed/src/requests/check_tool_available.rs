//! `NotebookRequest::CheckToolAvailable` handler.

use crate::protocol::NotebookResponse;

pub(crate) async fn handle(tool: String) -> NotebookResponse {
    let available = match tool.as_str() {
        "deno" => kernel_launch::tools::check_deno_available_without_bootstrap().await,
        _ => false,
    };
    NotebookResponse::ToolAvailable { available }
}
