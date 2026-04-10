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
                    "No active notebook session. Call open_notebook or create_notebook first.",
                )
            }
        }
    }};
}

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

/// Build `_meta` that opts a tool out of deferred-tool lists in Claude clients.
/// Claude Code / Desktop / Cowork defer all MCP tools by default; setting
/// `"anthropic/alwaysLoad": true` makes the tool immediately available
/// without requiring a ToolSearch round-trip.
fn always_load_meta() -> Meta {
    let mut meta = serde_json::Map::new();
    meta.insert("anthropic/alwaysLoad".to_string(), serde_json::json!(true));
    Meta(meta)
}

mod cell_crud;
mod cell_meta;
pub(crate) mod cell_read;
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
            "List all notebook sessions running in the daemon. Returns notebooks opened by any user or agent. Use open_notebook(notebook) to connect to one as your active session.",
            schema_for::<EmptyParams>(),
        )
        .annotate(ToolAnnotations::new().read_only(true).open_world(false))
        .with_meta(always_load_meta()),
        Tool::new(
            "open_notebook",
            "Open a notebook by file path or connect to a running session by ID. Accepts a file path (e.g. '~/analysis.ipynb') or a notebook_id from list_active_notebooks. Makes it your active session.",
            schema_for::<session::OpenNotebookParams>(),
        )
        .annotate(
            ToolAnnotations::new()
                .destructive(false)
                .idempotent(true)
                .open_world(true),
        )
        .with_meta(always_load_meta()),
        Tool::new(
            "create_notebook",
            "Create a new notebook, making it your active session. Notebooks are ephemeral by default (in-memory only) — use save_notebook(path) to persist to disk. Set ephemeral=false for session-restorable persistence. Supports uv, conda, or pixi via package_manager param.",
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
            "launch_app",
            "Launch the nteract desktop app for the user, showing the current notebook. The notebook must be running in the daemon.",
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
            "Execute all code cells in order. With wait=true (default), waits for completion and returns per-cell outputs with structured content. With wait=false, queues cells and returns immediately.",
            schema_for::<execution::RunAllCellsParams>(),
        )
        .annotate(ToolAnnotations::new().destructive(true).open_world(true))
        .with_meta(app_tool_meta()),
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
            "Add a package dependency (e.g. 'pandas>=2.0'). When a project file (pixi.toml, pyproject.toml) exists, promotes the dependency to the project file via 'pixi add' or 'uv add'. Otherwise stores in notebook metadata. Use after='sync' or after='restart' to apply.",
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
            "Remove a package dependency. When a project file exists, runs 'pixi remove' or 'uv remove'. Otherwise removes from notebook metadata. Requires restart_kernel() to take effect.",
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
        "open_notebook" => session::open_notebook(server, request).await,
        // Backward compat: join_notebook routes to open_notebook
        "join_notebook" => session::open_notebook(server, request).await,
        "create_notebook" => session::create_notebook(server, request).await,
        "save_notebook" => session::save_notebook(server, request).await,
        "launch_app" => session::show_notebook(server, request).await,
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

/// Helper: extract a string argument.
pub fn arg_str<'a>(request: &'a CallToolRequestParams, key: &str) -> Option<&'a str> {
    request
        .arguments
        .as_ref()
        .and_then(|args| args.get(key))
        .and_then(|v| v.as_str())
}

/// Helper: extract a boolean argument, tolerating string "true"/"false".
///
/// Claude Code's MCP client has a known bug where boolean params are sometimes
/// serialized as strings (e.g., `"true"` instead of `true`). This affects
/// tools with `required` fields inconsistently.
/// See: https://github.com/anthropics/claude-code/issues/32524
pub fn arg_bool(request: &CallToolRequestParams, key: &str) -> Option<bool> {
    let val = request.arguments.as_ref()?.get(key)?;
    if let Some(b) = val.as_bool() {
        return Some(b);
    }
    match val.as_str() {
        Some("true") => {
            tracing::warn!(
                "[mcp] Boolean param '{key}' arrived as string \"true\" (claude-code#32524)"
            );
            Some(true)
        }
        Some("false") => {
            tracing::warn!(
                "[mcp] Boolean param '{key}' arrived as string \"false\" (claude-code#32524)"
            );
            Some(false)
        }
        _ => None,
    }
}

/// Helper: extract a string array argument, tolerating JSON-encoded strings.
///
/// Same upstream bug as `arg_bool` — Claude Code may serialize arrays as
/// JSON-encoded strings (e.g., `"[\"numpy\"]"` instead of `["numpy"]`).
/// See: https://github.com/anthropics/claude-code/issues/32524
pub fn arg_string_array(request: &CallToolRequestParams, key: &str) -> Option<Vec<String>> {
    let val = request.arguments.as_ref()?.get(key)?;
    if let Some(arr) = val.as_array() {
        return Some(
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
        );
    }
    if let Some(s) = val.as_str() {
        if let Ok(parsed) = serde_json::from_str::<Vec<String>>(s) {
            tracing::warn!("[mcp] Array param '{key}' arrived as JSON string (claude-code#32524)");
            return Some(parsed);
        }
    }
    None
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

    // Build structured content directly from manifest Values + blob URLs.
    // No blob fetches — inline ContentRefs pass through, blobs become URLs.
    let cell_snapshot = handle.get_cell(&result.cell_id);
    let structured_content = if let Some(snap) = cell_snapshot {
        if snap.outputs.is_empty() {
            None
        } else {
            let ec_str = cell_read::get_cell_execution_count_from_runtime(handle, &snap.id);
            let ec: Option<i64> = if ec_str.is_empty() {
                None
            } else {
                ec_str.parse().ok()
            };
            Some(crate::structured::cell_structured_content_from_manifests(
                &snap.id,
                &snap.cell_type,
                &snap.source,
                &snap.outputs,
                ec,
                &result.status,
                &server.blob_base_url,
            ))
        }
    } else {
        None
    };

    let mut call_result = CallToolResult::success(items);
    call_result.structured_content = structured_content;
    Ok(call_result)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn make_request(args: serde_json::Value) -> CallToolRequestParams {
        serde_json::from_value(serde_json::json!({
            "name": "test",
            "arguments": args,
        }))
        .unwrap()
    }

    #[test]
    fn arg_bool_json_true() {
        let req = make_request(serde_json::json!({"flag": true}));
        assert_eq!(arg_bool(&req, "flag"), Some(true));
    }

    #[test]
    fn arg_bool_json_false() {
        let req = make_request(serde_json::json!({"flag": false}));
        assert_eq!(arg_bool(&req, "flag"), Some(false));
    }

    #[test]
    fn arg_bool_string_true() {
        let req = make_request(serde_json::json!({"flag": "true"}));
        assert_eq!(arg_bool(&req, "flag"), Some(true));
    }

    #[test]
    fn arg_bool_string_false() {
        let req = make_request(serde_json::json!({"flag": "false"}));
        assert_eq!(arg_bool(&req, "flag"), Some(false));
    }

    #[test]
    fn arg_bool_missing_key() {
        let req = make_request(serde_json::json!({"other": 1}));
        assert_eq!(arg_bool(&req, "flag"), None);
    }

    #[test]
    fn arg_bool_invalid_string() {
        let req = make_request(serde_json::json!({"flag": "yes"}));
        assert_eq!(arg_bool(&req, "flag"), None);
    }

    #[test]
    fn arg_bool_number() {
        let req = make_request(serde_json::json!({"flag": 1}));
        assert_eq!(arg_bool(&req, "flag"), None);
    }

    #[test]
    fn arg_bool_null() {
        let req = make_request(serde_json::json!({"flag": null}));
        assert_eq!(arg_bool(&req, "flag"), None);
    }

    #[test]
    fn arg_string_array_json_array() {
        let req = make_request(serde_json::json!({"deps": ["numpy", "pandas"]}));
        assert_eq!(
            arg_string_array(&req, "deps"),
            Some(vec!["numpy".to_string(), "pandas".to_string()])
        );
    }

    #[test]
    fn arg_string_array_string_coercion() {
        let req = make_request(serde_json::json!({"deps": "[\"numpy\", \"pandas\"]"}));
        assert_eq!(
            arg_string_array(&req, "deps"),
            Some(vec!["numpy".to_string(), "pandas".to_string()])
        );
    }

    #[test]
    fn arg_string_array_empty() {
        let req = make_request(serde_json::json!({"deps": []}));
        assert_eq!(arg_string_array(&req, "deps"), Some(vec![]));
    }

    #[test]
    fn arg_string_array_missing() {
        let req = make_request(serde_json::json!({"other": 1}));
        assert_eq!(arg_string_array(&req, "deps"), None);
    }

    #[test]
    fn arg_string_array_invalid_string() {
        let req = make_request(serde_json::json!({"deps": "not-json"}));
        assert_eq!(arg_string_array(&req, "deps"), None);
    }
}
