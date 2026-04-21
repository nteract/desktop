//! Execution tools: execute_cell, run_all_cells.

use std::time::Duration;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::execution;
use crate::formatting;
use crate::NteractMcp;

use super::{arg_bool, arg_str, tool_error, tool_success};

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

    // Verify cell exists
    if handle.get_cell(cell_id).is_none() {
        return tool_error(&format!("Cell not found: {cell_id}"));
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
    let run_exec = |cell_id: &str| -> Option<&notebook_doc::runtime_state::ExecutionState> {
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
            runtimed_client::output_resolver::resolve_cell_outputs_for_llm(
                output_manifests,
                &server.blob_base_url,
                &server.blob_store_path,
                comms,
            )
            .await
        } else {
            Vec::new()
        };

        // Text content: cell header + output text items.
        let cell_header = formatting::format_cell_header(
            &cell.id,
            "code",
            ec_str.as_deref(),
            Some(display_status),
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
                if let Some(cell_data) = wrapped.get("cell").cloned() {
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
