//! MCP tool definitions and dispatch.

use std::sync::Arc;

use rmcp::model::{CallToolRequestParams, CallToolResult, Content, Tool, ToolAnnotations};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::NteractMcp;

mod session;

/// Helper to generate a tool's input schema from a type.
fn schema_for<T: JsonSchema>() -> Arc<serde_json::Map<String, serde_json::Value>> {
    #[allow(clippy::unwrap_used)] // schemars always produces valid JSON
    let value = serde_json::to_value(schemars::schema_for!(T)).unwrap();
    #[allow(clippy::unwrap_used)]
    Arc::new(value.as_object().cloned().unwrap_or_default())
}

/// Empty params for tools that take no arguments.
#[derive(Debug, Deserialize, JsonSchema)]
struct EmptyParams {}

/// Return all registered tools.
pub fn all_tools() -> Vec<Tool> {
    vec![
        // -- Session management --
        Tool::new(
            "list_active_notebooks",
            "List all open notebook sessions. Returns notebooks currently open by users or other agents. Use join_notebook(notebook_id) to connect to one.",
            schema_for::<EmptyParams>(),
        )
        .annotate(ToolAnnotations::new().read_only(true)),
        Tool::new(
            "join_notebook",
            "Connect to an existing notebook session by ID. The notebook_id comes from list_active_notebooks.",
            schema_for::<session::JoinNotebookParams>(),
        )
        .annotate(ToolAnnotations::new().destructive(false)),
        Tool::new(
            "open_notebook",
            "Open a notebook file from disk. Creates a session and connects to it.",
            schema_for::<session::OpenNotebookParams>(),
        )
        .annotate(ToolAnnotations::new().destructive(false)),
    ]
}

/// Dispatch a tool call to its handler.
pub async fn dispatch(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    match request.name.as_ref() {
        "list_active_notebooks" => session::list_active_notebooks(server).await,
        "join_notebook" => session::join_notebook(server, request).await,
        "open_notebook" => session::open_notebook(server, request).await,
        _ => Err(McpError::invalid_params(
            format!("Unknown tool: {}", request.name),
            None,
        )),
    }
}

/// Helper: extract a typed argument or return a default.
pub fn arg_str<'a>(request: &'a CallToolRequestParams, key: &str) -> Option<&'a str> {
    request
        .arguments
        .as_ref()
        .and_then(|args| args.get(key))
        .and_then(|v| v.as_str())
}

/// Helper: create a text error result.
pub fn tool_error(msg: &str) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::error(vec![Content::text(msg.to_string())]))
}

/// Helper: create a text success result.
pub fn tool_success(msg: &str) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![Content::text(
        msg.to_string(),
    )]))
}
