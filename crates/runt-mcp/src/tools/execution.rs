//! Execution tools: execute_cell, run_all_cells, get_results.

use std::time::Duration;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::ErrorData as McpError;
use runtimed_client::output_resolver;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::execution;
use crate::formatting;
use crate::NteractMcp;

use super::{arg_bool, arg_str, assert_cell_exists, tool_error, tool_success};

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExecuteCellParams {
    /// The cell ID to execute.
    pub cell_id: String,
    /// Max seconds to wait; returns partial results if exceeded.
    #[serde(default)]
    pub timeout_secs: Option<f64>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunAllCellsParams {
    /// Max seconds to wait for all cells to finish. Default: 300.
    #[serde(default)]
    pub timeout_secs: Option<f64>,
    /// If true (default), wait for all cells to finish and return outputs.
    /// If false, queue cells and return immediately.
    #[serde(default)]
    pub wait: Option<bool>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetResultsParams {
    /// The execution ID returned by `execute_cell`, `set_cell(and_run=true)`,
    /// `create_cell(and_run=true)`, or `run_all_cells`.
    pub execution_id: String,
    /// Return unabridged output text (like `get_cell(full_output=true)`).
    /// Default: false (preview mode to protect context budget).
    #[serde(default)]
    pub full_output: Option<bool>,
}

/// Execute a cell and return results (with structured content for MCP Apps).
pub async fn execute_cell(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let cell_id = arg_str(request, "cell_id")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: cell_id", None))?;

    let handle = require_handle!(server);

    let timeout_secs = request
        .arguments
        .as_ref()
        .and_then(|a| a.get("timeout_secs"))
        .and_then(|v| v.as_f64())
        .unwrap_or(30.0);

    assert_cell_exists(&handle, cell_id)?;

    // Only code cells can be executed. Markdown/raw cells get queued by the
    // daemon but the kernel never processes them, so the agent would see
    // "running" forever with no outputs. Fail early with a clear message.
    let cell_type = handle.get_cell_type(cell_id).unwrap_or_default();
    if cell_type != "code" {
        return tool_error(&format!(
            "Cannot execute cell '{cell_id}' of type '{cell_type}'. \
             Only 'code' cells can be executed."
        ));
    }

    let peer_label = server.get_peer_label().await;
    crate::presence::emit_focus(&handle, cell_id, &peer_label).await;

    let result = execution::execute_and_wait(
        &handle,
        cell_id,
        Duration::from_secs_f64(timeout_secs),
        &server.blob_base_url,
        &server.blob_store_path,
    )
    .await;

    super::build_execution_result(&result, &handle, server).await
}

/// Execute all code cells in order.
///
/// With `wait=true` (default): waits for completion and returns per-cell outputs
/// with structured content, like `execute_cell` but for every code cell.
///
/// With `wait=false`: queues all cells and returns immediately.
pub async fn run_all_cells(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let handle = require_handle!(server);

    let wait = arg_bool(request, "wait").unwrap_or(true);

    let timeout_secs = request
        .arguments
        .as_ref()
        .and_then(|a| a.get("timeout_secs"))
        .and_then(|v| v.as_f64())
        .unwrap_or(300.0);

    // Fire-and-forget: queue cells and return immediately.
    if !wait {
        let result = execution::run_all_and_queue(&handle).await;
        if result.status == "error" {
            return tool_error("Failed to queue cells for execution");
        }
        let n = result.cell_execution_ids.len();
        let mut lines = vec![format!("Queued {n} cells for execution")];
        for (cell_id, exec_id) in &result.cell_execution_ids {
            lines.push(format!("  {cell_id} → {exec_id}"));
        }
        return tool_success(&lines.join("\n"));
    }

    // Wait mode: run all cells and collect outputs.
    let result = execution::run_all_and_wait(&handle, Duration::from_secs_f64(timeout_secs)).await;

    let cells = handle.get_cells();
    let runtime_state = handle.get_runtime_state().ok();

    // Look up this run's execution state for a given cell.
    let run_exec = |cell_id: &str| -> Option<&runtime_doc::ExecutionState> {
        let eid = result.cell_execution_ids.get(cell_id)?;
        runtime_state.as_ref()?.executions.get(eid.as_str())
    };

    // Count code cells by status for the header.
    let mut succeeded = 0usize;
    let mut errored = 0usize;
    let mut cancelled = 0usize;
    let mut running = 0usize;
    let mut queued = 0usize;

    for cell in &cells {
        if cell.cell_type != "code" {
            continue;
        }
        if let Some(exec) = run_exec(&cell.id) {
            match exec.status.as_str() {
                "done" => succeeded += 1,
                "error" => {
                    if exec.execution_count.is_none() {
                        cancelled += 1;
                    } else {
                        errored += 1;
                    }
                }
                "running" => running += 1,
                "queued" => queued += 1,
                _ => {}
            }
        }
    }

    // Build status header line.
    let header = match result.status.as_str() {
        "timed_out" => {
            let done = succeeded + errored;
            let total = done + cancelled + running + queued;
            let mut parts = vec![format!("{done} completed")];
            if running > 0 {
                parts.push(format!("{running} running"));
            }
            if queued > 0 {
                parts.push(format!("{queued} queued"));
            }
            format!("Execution timed out ({total} cells: {})", parts.join(", "))
        }
        "error" => {
            let mut parts = Vec::new();
            if succeeded > 0 {
                parts.push(format!("{succeeded} succeeded"));
            }
            if errored > 0 {
                parts.push(format!("{errored} errored"));
            }
            if cancelled > 0 {
                parts.push(format!("{cancelled} cancelled"));
            }
            format!("Execution error ({})", parts.join(", "))
        }
        _ => {
            format!("Execution completed ({succeeded} succeeded)")
        }
    };

    // Build per-cell output content.
    let comms = runtime_state.as_ref().map(|rs| &rs.comms);
    let mut content_items = vec![rmcp::model::Content::text(header.clone())];
    let mut structured_cells: Vec<serde_json::Value> = Vec::new();

    for cell in &cells {
        if cell.cell_type != "code" {
            continue;
        }

        let exec = match run_exec(&cell.id) {
            Some(e) => e,
            None => continue,
        };

        let display_status = match exec.status.as_str() {
            "error" if exec.execution_count.is_none() => "cancelled",
            other => other,
        };
        let ec_str = exec.execution_count.map(|c| c.to_string());

        // Resolve outputs from the execution's output manifests.
        let output_manifests = &exec.outputs;
        let outputs = if !output_manifests.is_empty() {
            // Batch execute path — always preview mode. No per-cell opt-out.
            runtimed_client::output_resolver::resolve_cell_outputs_for_llm(
                output_manifests,
                runtimed_client::output_resolver::ResolveCtx {
                    blob_base_url: server.blob_base_url.as_deref(),
                    blob_store_path: server.blob_store_path.as_deref(),
                    comms,
                    ..Default::default()
                },
            )
            .await
        } else {
            Vec::new()
        };

        // Text content: cell header + output text items.
        let eid = result.cell_execution_ids.get(&cell.id).map(|s| s.as_str());
        let cell_header = formatting::format_cell_header(
            &cell.id,
            "code",
            ec_str.as_deref(),
            Some(display_status),
            eid,
        );
        content_items.push(rmcp::model::Content::text(cell_header));
        content_items.extend(formatting::outputs_to_content_items(&outputs));

        // Structured content for MCP Apps: use manifests from the cell snapshot
        // (which include ContentRef entries needed for structured rendering).
        // Extract the inner "cell" object — cell_structured_content_from_manifests
        // returns {"cell": {...}, "blob_base_url": "..."} but the multi-cell
        // wrapper expects CellData directly in the cells[] array.
        // Outputs live in RuntimeStateDoc, keyed by execution_id; fetch them
        // alongside the snapshot.
        let cell_snapshot = handle.get_cell(&cell.id);
        if let Some(snap) = cell_snapshot {
            let snap_outputs = handle.get_cell_outputs(&cell.id).unwrap_or_default();
            if !snap_outputs.is_empty() {
                let wrapped = crate::structured::cell_structured_content_from_manifests(
                    &snap.id,
                    &snap.cell_type,
                    &snap.source,
                    &snap_outputs,
                    exec.execution_count,
                    display_status,
                    &server.blob_base_url,
                );
                if let Some(mut cell_data) = wrapped.get("cell").cloned() {
                    if let Some(eid) = eid {
                        if let Some(obj) = cell_data.as_object_mut() {
                            obj.insert(
                                "execution_id".to_string(),
                                serde_json::Value::String(eid.to_string()),
                            );
                        }
                    }
                    structured_cells.push(cell_data);
                }
            }
        }
    }

    let mut call_result = rmcp::model::CallToolResult::success(content_items);

    // Wrap structured content as {"cells": [...]} for multi-cell responses.
    if !structured_cells.is_empty() {
        let mut wrapper = serde_json::json!({
            "cells": structured_cells,
        });
        if let Some(base) = &server.blob_base_url {
            wrapper["blob_base_url"] = serde_json::Value::String(base.clone());
        }
        call_result.structured_content = Some(wrapper);
    }

    Ok(call_result)
}

/// Get outputs for a specific execution by ID.
///
/// Standalone read-only tool — no cell_id needed. Looks up the execution
/// in RuntimeStateDoc, renders status prominently so agents know whether
/// outputs are partial (still running) or complete.
pub async fn get_results(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let execution_id = arg_str(request, "execution_id").ok_or_else(|| {
        McpError::invalid_params("Missing required parameter: execution_id", None)
    })?;
    let full_output = arg_bool(request, "full_output").unwrap_or(false);

    let handle = {
        let guard = server.session.read().await;
        guard.as_ref().map(|session| session.handle.clone())
    };

    if let Some(handle) = handle.as_ref() {
        let runtime_state = handle.get_runtime_state().map_err(|_| {
            McpError::internal_error("Failed to read RuntimeStateDoc".to_string(), None)
        })?;
        if let Some(exec) = runtime_state.executions.get(execution_id) {
            let cell = handle.get_cell(&exec.cell_id);
            return render_execution_result(
                server,
                execution_id,
                exec,
                Some(&runtime_state.comms),
                cell,
                full_output,
            )
            .await;
        }
    }

    let store =
        runtimed_client::execution_store::ExecutionStore::new(server.execution_store_path.clone());
    if let Some(record) = store.read_record(execution_id).await {
        let exec = runtime_doc::ExecutionState {
            cell_id: record.cell_id.clone().unwrap_or_default(),
            status: record.status,
            execution_count: record.execution_count,
            success: record.success,
            outputs: record.outputs,
            source: record.source,
            seq: record.seq,
        };
        let cell = handle
            .as_ref()
            .and_then(|handle| handle.get_cell(&exec.cell_id));
        return render_execution_result(server, execution_id, &exec, None, cell, full_output).await;
    }

    tool_error(&format!(
        "Execution not found: {execution_id}. It may have been evicted and no durable result record was found."
    ))
}

async fn render_execution_result(
    server: &NteractMcp,
    execution_id: &str,
    exec: &runtime_doc::ExecutionState,
    comms: Option<&std::collections::HashMap<String, runtime_doc::CommDocEntry>>,
    cell: Option<notebook_doc::CellSnapshot>,
    full_output: bool,
) -> Result<CallToolResult, McpError> {
    // Determine display status with clear indication of completeness
    let (display_status, is_terminal) = match exec.status.as_str() {
        "done" => ("done", true),
        "error" if exec.execution_count.is_none() => ("cancelled", true),
        "error" => ("error", true),
        "running" => ("running (partial — outputs may be incomplete)", false),
        "queued" => ("queued (no outputs yet)", false),
        other => (other, false),
    };

    let ec_str = exec.execution_count.map(|c| c.to_string());

    // Build header with execution state front and center
    let header = formatting::format_cell_header(
        &exec.cell_id,
        "code",
        ec_str.as_deref(),
        Some(display_status),
        Some(execution_id),
    );

    // Resolve outputs from the execution's manifests
    let outputs = if !exec.outputs.is_empty() {
        output_resolver::resolve_cell_outputs_for_llm(
            &exec.outputs,
            output_resolver::ResolveCtx {
                blob_base_url: server.blob_base_url.as_deref(),
                blob_store_path: server.blob_store_path.as_deref(),
                comms,
                length: if full_output {
                    output_resolver::OutputLength::Full
                } else {
                    output_resolver::OutputLength::Preview
                },
            },
        )
        .await
    } else {
        Vec::new()
    };

    let mut items = vec![rmcp::model::Content::text(header)];

    if !is_terminal && outputs.is_empty() {
        // No outputs yet — make it crystal clear
        items.push(rmcp::model::Content::text(format!(
            "Status: {display_status}. No outputs available yet."
        )));
    } else if !is_terminal {
        items.push(rmcp::model::Content::text(format!(
            "⚠ Status: {display_status}. Outputs below may be incomplete."
        )));
        items.extend(formatting::outputs_to_content_items(&outputs));
    } else {
        items.extend(formatting::outputs_to_content_items(&outputs));
    }

    // Build structured content from the execution's output manifests
    let fallback_source = exec.source.as_deref().unwrap_or_default();
    let fallback_cell = notebook_doc::CellSnapshot {
        id: exec.cell_id.clone(),
        cell_type: "code".to_string(),
        position: String::new(),
        source: fallback_source.to_string(),
        execution_count: exec
            .execution_count
            .map(|count| count.to_string())
            .unwrap_or_else(|| "null".to_string()),
        metadata: serde_json::json!({}),
        resolved_assets: std::collections::HashMap::new(),
    };
    let snap = cell.unwrap_or(fallback_cell);
    let mut structured_content = if exec.outputs.is_empty() {
        None
    } else {
        let wrapped = crate::structured::cell_structured_content_from_manifests(
            &snap.id,
            &snap.cell_type,
            &snap.source,
            &exec.outputs,
            exec.execution_count,
            display_status,
            &server.blob_base_url,
        );
        wrapped.get("cell").cloned().map(|mut cell_data| {
            if let Some(obj) = cell_data.as_object_mut() {
                obj.insert(
                    "execution_id".to_string(),
                    serde_json::Value::String(execution_id.to_string()),
                );
            }
            // Wrap as top-level with blob_base_url
            let mut top = serde_json::json!({ "cell": cell_data });
            if let Some(base) = &server.blob_base_url {
                top["blob_base_url"] = serde_json::Value::String(base.clone());
            }
            top
        })
    };

    let mut call_result = rmcp::model::CallToolResult::success(items);
    call_result.structured_content = structured_content.take();
    Ok(call_result)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn make_request(args: serde_json::Value) -> CallToolRequestParams {
        serde_json::from_value(serde_json::json!({
            "name": "get_results",
            "arguments": args,
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn get_results_reads_durable_store_without_active_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = runtimed_client::execution_store::ExecutionStore::new(tmp.path());
        store
            .write_record(runtimed_client::execution_store::ExecutionRecord {
                schema_version: runtimed_client::execution_store::EXECUTION_RECORD_SCHEMA_VERSION,
                execution_id: "exec-durable".to_string(),
                context_kind: "notebook".to_string(),
                context_id: "/tmp/notebook.ipynb".to_string(),
                notebook_path: Some("/tmp/notebook.ipynb".to_string()),
                cell_id: Some("cell-1".to_string()),
                status: "done".to_string(),
                success: Some(true),
                execution_count: Some(3),
                source: Some("print('hi')".to_string()),
                seq: Some(0),
                outputs: vec![serde_json::json!({
                    "output_type": "stream",
                    "name": "stdout",
                    "text": {"inline": "hi\n"}
                })],
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            })
            .await
            .unwrap();

        let server = NteractMcp::new(PathBuf::from("/tmp/missing.sock"), None, None)
            .with_execution_store_path(Some(tmp.path().to_path_buf()));
        let result = get_results(
            &server,
            &make_request(serde_json::json!({"execution_id": "exec-durable"})),
        )
        .await
        .unwrap();

        assert_ne!(result.is_error, Some(true));
        let content = serde_json::to_string(&result.content).unwrap();
        assert!(content.contains("exec-durable"));
        assert!(content.contains("hi"));
        assert_eq!(
            result.structured_content.unwrap()["cell"]["execution_id"],
            "exec-durable"
        );
    }

    #[tokio::test]
    async fn get_results_missing_durable_record_omits_unknown_cell_recovery_hint() {
        let tmp = tempfile::TempDir::new().unwrap();
        let server = NteractMcp::new(PathBuf::from("/tmp/missing.sock"), None, None)
            .with_execution_store_path(Some(tmp.path().to_path_buf()));
        let result = get_results(
            &server,
            &make_request(serde_json::json!({"execution_id": "exec-missing"})),
        )
        .await
        .unwrap();

        assert_eq!(result.is_error, Some(true));
        let content = serde_json::to_string(&result.content).unwrap();
        assert!(content.contains("Execution not found"));
        assert!(!content.contains("get_cell("));
    }
}
