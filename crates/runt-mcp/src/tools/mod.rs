//! MCP tool definitions and dispatch.

use std::sync::Arc;

use rmcp::model::{CallToolRequestParams, CallToolResult, Content, Meta, Tool, ToolAnnotations};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::NteractMcp;

/// Acquire the active session's `DocHandle`, or early-return a "no session" tool error.
/// Clones the handle and drops the session read-lock so other tools aren't blocked.
macro_rules! require_handle {
    ($server:expr) => {{
        let guard = $server.session.read().await;
        match guard.as_ref() {
            Some(s) => s.handle.clone(),
            None => {
                return $crate::tools::tool_error(
                    "No active notebook session. Call join_notebook or open_notebook first.",
                )
            }
        }
    }};
}
pub(crate) use require_handle;

/// The MCP Apps resource URI for the output widget.
const OUTPUT_RESOURCE_URI: &str = "ui://nteract/output.html";

/// Build `_meta` for tools that produce structured content for the MCP Apps widget.
/// Wire format: `{ "ui": { "resourceUri": "ui://nteract/output.html" } }`
fn app_tool_meta() -> Meta {
    let mut meta = serde_json::Map::new();
    meta.insert(
        "ui".to_string(),
        serde_json::json!({ "resourceUri": OUTPUT_RESOURCE_URI }),
    );
    Meta(meta)
}

mod cell_crud;
mod cell_meta;
mod cell_read;
mod deps;
mod editing;
mod execution;
mod kernel;
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
///
/// Annotation semantics (from MCP spec):
/// - `read_only` — tool does not modify its environment
/// - `destructive` — tool may perform destructive (irreversible) updates
///   (only meaningful when read_only is false)
/// - `idempotent` — calling repeatedly with the same args has no additional effect
/// - `open_world` — tool interacts with external entities beyond the notebook
pub fn all_tools() -> Vec<Tool> {
    vec![
        // -- Session management --
        Tool::new(
            "list_active_notebooks",
            "List all open notebook sessions. Returns notebooks currently open by users or other agents. Use join_notebook(notebook_id) to connect to one.",
            schema_for::<EmptyParams>(),
        )
        .annotate(ToolAnnotations::new().read_only(true).open_world(false)),
        Tool::new(
            "join_notebook",
            "Connect to an existing notebook session by ID. The notebook_id comes from list_active_notebooks.",
            schema_for::<session::JoinNotebookParams>(),
        )
        .annotate(
            ToolAnnotations::new()
                .destructive(false)
                .idempotent(true)
                .open_world(false),
        ),
        Tool::new(
            "open_notebook",
            "Open a notebook file from disk. Creates a session and connects to it.",
            schema_for::<session::OpenNotebookParams>(),
        )
        .annotate(
            ToolAnnotations::new()
                .destructive(false)
                .idempotent(true)
                .open_world(true),
        ),
        Tool::new(
            "create_notebook",
            "Create a new notebook with optional pre-installed dependencies. The kernel starts automatically. Call save_notebook(path) to persist to disk.",
            schema_for::<session::CreateNotebookParams>(),
        )
        .annotate(ToolAnnotations::new().destructive(false).open_world(false)),
        Tool::new(
            "save_notebook",
            "Save notebook to disk. The daemon automatically re-keys ephemeral rooms to the saved file path.",
            schema_for::<session::SaveNotebookParams>(),
        )
        .annotate(
            ToolAnnotations::new()
                .destructive(false)
                .idempotent(true)
                .open_world(true),
        ),
        Tool::new(
            "show_notebook",
            "Open the notebook in the nteract desktop app. The notebook must be running in the daemon.",
            schema_for::<session::ShowNotebookParams>(),
        )
        .annotate(ToolAnnotations::new().read_only(true).open_world(false)),
        // -- Cell read --
        Tool::new(
            "get_cell",
            "Get a cell's source and outputs by ID.",
            schema_for::<cell_read::GetCellParams>(),
        )
        .annotate(ToolAnnotations::new().read_only(true).open_world(false)),
        Tool::new(
            "get_all_cells",
            "Get all cells. Use summary (default) for discovery, get_cell() for details. Formats: 'summary', 'json', 'rich'.",
            schema_for::<cell_read::GetAllCellsParams>(),
        )
        .annotate(ToolAnnotations::new().read_only(true).open_world(false)),
        // -- Cell CRUD --
        Tool::new(
            "create_cell",
            "Create a cell, optionally executing it.",
            schema_for::<cell_crud::CreateCellParams>(),
        )
        .annotate(ToolAnnotations::new().destructive(false).open_world(false))
        .with_meta(app_tool_meta()),
        Tool::new(
            "set_cell",
            "Update a cell's source and/or type. Use replace_match for targeted edits.",
            schema_for::<cell_crud::SetCellParams>(),
        )
        .annotate(ToolAnnotations::new().destructive(false).open_world(false))
        .with_meta(app_tool_meta()),
        Tool::new(
            "delete_cell",
            "Delete a cell by ID.",
            schema_for::<cell_crud::DeleteCellParams>(),
        )
        .annotate(ToolAnnotations::new().destructive(true).open_world(false)),
        Tool::new(
            "move_cell",
            "Move a cell to a new position.",
            schema_for::<cell_crud::MoveCellParams>(),
        )
        .annotate(
            ToolAnnotations::new()
                .destructive(false)
                .idempotent(true)
                .open_world(false),
        ),
        Tool::new(
            "clear_outputs",
            "Clear cell outputs. Pass cell_ids to clear specific cells, or omit to clear ALL outputs (destructive).",
            schema_for::<cell_crud::ClearOutputsParams>(),
        )
        .annotate(
            ToolAnnotations::new()
                .destructive(true)
                .idempotent(true)
                .open_world(false),
        ),
        // -- Cell metadata --
        Tool::new(
            "add_cell_tags",
            "Add tags to a cell's metadata. Existing tags are preserved.",
            schema_for::<cell_meta::AddCellTagsParams>(),
        )
        .annotate(
            ToolAnnotations::new()
                .destructive(false)
                .idempotent(true)
                .open_world(false),
        ),
        Tool::new(
            "remove_cell_tags",
            "Remove tags from a cell's metadata.",
            schema_for::<cell_meta::RemoveCellTagsParams>(),
        )
        .annotate(
            ToolAnnotations::new()
                .destructive(true)
                .idempotent(true)
                .open_world(false),
        ),
        Tool::new(
            "set_cells_source_hidden",
            "Hide or show the source (code input) of one or more cells.",
            schema_for::<cell_meta::SetCellsSourceHiddenParams>(),
        )
        .annotate(
            ToolAnnotations::new()
                .destructive(false)
                .idempotent(true)
                .open_world(false),
        ),
        Tool::new(
            "set_cells_outputs_hidden",
            "Hide or show the outputs of one or more cells.",
            schema_for::<cell_meta::SetCellsOutputsHiddenParams>(),
        )
        .annotate(
            ToolAnnotations::new()
                .destructive(false)
                .idempotent(true)
                .open_world(false),
        ),
        // -- Execution --
        Tool::new(
            "execute_cell",
            "Execute a cell. Returns partial results if timeout exceeded.",
            schema_for::<execution::ExecuteCellParams>(),
        )
        .annotate(ToolAnnotations::new().destructive(true).open_world(true))
        .with_meta(app_tool_meta()),
        Tool::new(
            "run_all_cells",
            "Queue all code cells for execution. Use get_all_cells() to see results.",
            schema_for::<EmptyParams>(),
        )
        .annotate(ToolAnnotations::new().destructive(true).open_world(true)),
        // -- Kernel --
        Tool::new(
            "interrupt_kernel",
            "Interrupt the currently executing cell.",
            schema_for::<EmptyParams>(),
        )
        .annotate(
            ToolAnnotations::new()
                .destructive(true)
                .idempotent(true)
                .open_world(false),
        ),
        Tool::new(
            "restart_kernel",
            "Restart kernel, clearing all state. Use after dependency changes.",
            schema_for::<EmptyParams>(),
        )
        .annotate(ToolAnnotations::new().destructive(true).open_world(false)),
        // -- Dependencies --
        Tool::new(
            "add_dependency",
            "Add a package dependency (e.g. 'pandas>=2.0'). Call sync_environment() to install.",
            schema_for::<deps::AddDependencyParams>(),
        )
        .annotate(
            ToolAnnotations::new()
                .destructive(false)
                .idempotent(true)
                .open_world(false),
        ),
        Tool::new(
            "remove_dependency",
            "Remove a package dependency. Requires restart_kernel() to take effect.",
            schema_for::<deps::RemoveDependencyParams>(),
        )
        .annotate(
            ToolAnnotations::new()
                .destructive(true)
                .idempotent(true)
                .open_world(false),
        ),
        Tool::new(
            "get_dependencies",
            "Get the notebook's declared dependencies and any pre-installed packages available in the environment.",
            schema_for::<deps::GetDependenciesParams>(),
        )
        .annotate(ToolAnnotations::new().read_only(true).open_world(false)),
        Tool::new(
            "sync_environment",
            "Hot-install new dependencies without restarting. Use restart_kernel() if this fails.",
            schema_for::<EmptyParams>(),
        )
        .annotate(ToolAnnotations::new().destructive(false).open_world(true)),
        // -- Editing --
        Tool::new(
            "replace_match",
            "Replace matched text in a cell. Prefer this for simple, targeted edits. Use context_before/context_after to disambiguate when match appears multiple times.",
            schema_for::<editing::ReplaceMatchParams>(),
        )
        .annotate(ToolAnnotations::new().destructive(false).open_world(false))
        .with_meta(app_tool_meta()),
        Tool::new(
            "replace_regex",
            "Replace a regex-matched span (fancy-regex engine, Rust). Use for anchors, lookarounds, or zero-width insertions. Fails if 0 or >1 matches. Replacement content is literal (no escape interpretation — use actual newline chars, not \\n).",
            schema_for::<editing::ReplaceRegexParams>(),
        )
        .annotate(ToolAnnotations::new().destructive(false).open_world(false))
        .with_meta(app_tool_meta()),
    ]
}

/// Dispatch a tool call to its handler.
pub async fn dispatch(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    // Check daemon health before dispatching
    {
        let state = server.daemon_state().read().await;
        if let Some(msg) = state.reconnecting_message() {
            return tool_error(&msg);
        }
    }

    match request.name.as_ref() {
        // Session
        "list_active_notebooks" => session::list_active_notebooks(server).await,
        "join_notebook" => session::join_notebook(server, request).await,
        "open_notebook" => session::open_notebook(server, request).await,
        "create_notebook" => session::create_notebook(server, request).await,
        "save_notebook" => session::save_notebook(server, request).await,
        "show_notebook" => session::show_notebook(server, request).await,
        // Cell read
        "get_cell" => cell_read::get_cell(server, request).await,
        "get_all_cells" => cell_read::get_all_cells(server, request).await,
        // Cell CRUD
        "create_cell" => cell_crud::create_cell(server, request).await,
        "set_cell" => cell_crud::set_cell(server, request).await,
        "delete_cell" => cell_crud::delete_cell(server, request).await,
        "move_cell" => cell_crud::move_cell(server, request).await,
        "clear_outputs" => cell_crud::clear_outputs(server, request).await,
        // Cell metadata
        "add_cell_tags" => cell_meta::add_cell_tags(server, request).await,
        "remove_cell_tags" => cell_meta::remove_cell_tags(server, request).await,
        "set_cells_source_hidden" => cell_meta::set_cells_source_hidden(server, request).await,
        "set_cells_outputs_hidden" => cell_meta::set_cells_outputs_hidden(server, request).await,
        // Execution
        "execute_cell" => execution::execute_cell(server, request).await,
        "run_all_cells" => execution::run_all_cells(server, request).await,
        // Kernel
        "interrupt_kernel" => kernel::interrupt_kernel(server, request).await,
        "restart_kernel" => kernel::restart_kernel(server, request).await,
        // Dependencies
        "add_dependency" => deps::add_dependency(server, request).await,
        "remove_dependency" => deps::remove_dependency(server, request).await,
        "get_dependencies" => deps::get_dependencies(server, request).await,
        "sync_environment" => deps::sync_environment(server, request).await,
        // Editing
        "replace_match" => editing::replace_match(server, request).await,
        "replace_regex" => editing::replace_regex(server, request).await,
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

/// Build a `CallToolResult` from an execution result, including structured content
/// for the MCP Apps widget. Shared by cell_crud, editing, and execution tools.
pub async fn build_execution_result(
    result: &crate::execution::ExecutionResult,
    handle: &notebook_sync::handle::DocHandle,
    server: &NteractMcp,
) -> Result<CallToolResult, McpError> {
    let header = crate::formatting::format_cell_header(
        &result.cell_id,
        "code",
        result.execution_count.as_deref(),
        Some(&result.status),
    );

    let mut items = vec![Content::text(header)];
    items.extend(crate::formatting::outputs_to_content_items(&result.outputs));

    // Build structured content for MCP Apps widget.
    // Only send structured content when there are outputs to render.
    let cell_snapshot = handle.get_cell(&result.cell_id);
    let structured_content = if let Some(snap) = cell_snapshot {
        if snap.outputs.is_empty() {
            None
        } else {
            let comms = handle.get_runtime_state().ok().map(|rs| rs.comms);
            let outputs = runtimed_client::output_resolver::resolve_cell_outputs(
                &snap.outputs,
                &server.blob_base_url,
                &server.blob_store_path,
                comms.as_ref(),
            )
            .await;
            let resolved = runtimed_client::resolved_output::ResolvedCell {
                id: snap.id,
                cell_type: snap.cell_type,
                position: snap.position,
                source: snap.source,
                execution_count: snap.execution_count.parse().ok(),
                outputs,
                metadata_json: serde_json::to_string(&snap.metadata).unwrap_or_default(),
            };
            Some(crate::structured::cell_structured_content(
                &resolved,
                &result.status,
            ))
        }
    } else {
        None
    };

    let mut call_result = CallToolResult::success(items);
    call_result.structured_content = structured_content;
    Ok(call_result)
}
