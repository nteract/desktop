//! Generate Automerge document fixtures for frontend (vitest) integration tests.
//!
//! Each scenario creates a NotebookDoc with daemon-authored mutations (outputs,
//! execution counts, etc.) and saves the full doc to `packages/runtimed/tests/fixtures/`.
//!
//! The frontend tests load these docs into a WASM "server" handle, then use
//! DirectTransport to sync to a fresh WASM client handle — driving the real
//! 2-party Automerge sync protocol through the SyncEngine pipeline.
//!
//! Run with:
//!   cargo test -p notebook-doc --test generate_fixtures -- --nocapture

use notebook_doc::{frame_types, NotebookDoc};
use serde_json::json;
use std::fs;
use std::path::PathBuf;

/// Directory where fixtures are written.
fn fixtures_dir() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir).join("../../packages/runtimed/tests/fixtures")
}

/// Frame: [type_byte, ...json_bytes]
fn make_broadcast_frame(broadcast: &serde_json::Value) -> Vec<u8> {
    let json_bytes = serde_json::to_vec(broadcast).unwrap();
    let mut frame = Vec::with_capacity(1 + json_bytes.len());
    frame.push(frame_types::BROADCAST);
    frame.extend_from_slice(&json_bytes);
    frame
}

/// Clear and recreate a scenario directory so stale files from previous runs
/// (e.g. renamed or removed broadcast frames) don't linger.
fn clean_scenario_dir(name: &str) -> PathBuf {
    let dir = fixtures_dir().join(name);
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write a scenario: manifest.json + doc.bin (saved Automerge document).
fn write_scenario(name: &str, daemon: &mut NotebookDoc, manifest: &serde_json::Value) {
    let dir = clean_scenario_dir(name);
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(manifest).unwrap(),
    )
    .unwrap();
    fs::write(dir.join("doc.bin"), daemon.save()).unwrap();
}

/// Write a scenario with broadcast frame files alongside the doc.
fn write_scenario_with_broadcasts(
    name: &str,
    daemon: &mut NotebookDoc,
    manifest: &serde_json::Value,
    broadcast_frames: &[Vec<u8>],
) {
    let dir = clean_scenario_dir(name);
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(manifest).unwrap(),
    )
    .unwrap();
    fs::write(dir.join("doc.bin"), daemon.save()).unwrap();
    for (i, frame) in broadcast_frames.iter().enumerate() {
        fs::write(dir.join(format!("broadcast_{i:03}.bin")), frame).unwrap();
    }
}

// ── Scenarios ────────────────────────────────────────────────────────

#[test]
fn scenario_output_streaming() {
    //! Daemon creates a cell, executes it, and streams stdout output.

    let mut daemon = NotebookDoc::new_with_actor("output-streaming", "fixture-output-streaming");
    daemon.add_cell(0, "cell-1", "code").unwrap();
    daemon
        .update_source("cell-1", "for i in range(3):\n    print(i)")
        .unwrap();

    // Simulate execution
    daemon.clear_outputs("cell-1").unwrap();
    daemon.set_execution_count("cell-1", "1").unwrap();

    let outputs = [
        json!({"output_type": "stream", "name": "stdout", "text": "0\n"}).to_string(),
        json!({"output_type": "stream", "name": "stdout", "text": "1\n"}).to_string(),
        json!({"output_type": "stream", "name": "stdout", "text": "2\n"}).to_string(),
    ];

    for output in &outputs {
        daemon.append_output("cell-1", output).unwrap();
    }

    // Generate broadcast frames (like real daemon sends alongside CRDT sync)
    let broadcast_frames: Vec<Vec<u8>> = outputs
        .iter()
        .enumerate()
        .map(|(i, o)| {
            make_broadcast_frame(&json!({
                "type": "Output",
                "cell_id": "cell-1",
                "execution_id": "exec-001",
                "output_type": "stream",
                "output_json": o,
                "output_index": i,
            }))
        })
        .collect();

    let manifest = json!({
        "scenario": "output_streaming",
        "description": "Daemon creates cell, executes, streams 3 stdout lines",
        "expected": {
            "cell_id": "cell-1",
            "source": "for i in range(3):\n    print(i)",
            "execution_count": "1",
            "output_count": 3,
            "outputs": outputs,
        }
    });

    write_scenario_with_broadcasts(
        "output_streaming",
        &mut daemon,
        &manifest,
        &broadcast_frames,
    );
}

#[test]
fn scenario_execution_with_error() {
    //! Daemon executes a cell that raises an error.

    let mut daemon = NotebookDoc::new_with_actor("error-execution", "fixture-error-execution");
    daemon.add_cell(0, "cell-1", "code").unwrap();
    daemon.update_source("cell-1", "1 / 0").unwrap();

    daemon.set_execution_count("cell-1", "1").unwrap();
    let error_output = json!({
        "output_type": "error",
        "ename": "ZeroDivisionError",
        "evalue": "division by zero",
        "traceback": [
            "\u{001b}[0;31m---------------------------------------------------------------------------\u{001b}[0m",
            "\u{001b}[0;31mZeroDivisionError\u{001b}[0m: division by zero"
        ]
    })
    .to_string();
    daemon.append_output("cell-1", &error_output).unwrap();

    let manifest = json!({
        "scenario": "execution_with_error",
        "description": "Daemon executes cell that raises ZeroDivisionError",
        "expected": {
            "cell_id": "cell-1",
            "source": "1 / 0",
            "execution_count": "1",
            "output_count": 1,
        }
    });

    write_scenario("execution_with_error", &mut daemon, &manifest);
}

#[test]
fn scenario_re_execution() {
    //! Cell executed twice. First outputs cleared, then new output written.

    let mut daemon = NotebookDoc::new_with_actor("re-execution", "fixture-re-execution");
    daemon.add_cell(0, "cell-1", "code").unwrap();
    daemon.update_source("cell-1", "print('hello')").unwrap();

    // First execution
    daemon.set_execution_count("cell-1", "1").unwrap();
    daemon
        .append_output(
            "cell-1",
            &json!({"output_type": "stream", "name": "stdout", "text": "hello\n"}).to_string(),
        )
        .unwrap();

    // Second execution: clear then new output
    daemon.clear_outputs("cell-1").unwrap();
    daemon.set_execution_count("cell-1", "2").unwrap();
    let output2 = json!({
        "output_type": "execute_result",
        "data": {"text/plain": "42"},
        "metadata": {},
        "execution_count": 2
    })
    .to_string();
    daemon.append_output("cell-1", &output2).unwrap();

    let manifest = json!({
        "scenario": "re_execution",
        "description": "Cell executed twice — only second execution's outputs remain",
        "expected": {
            "cell_id": "cell-1",
            "execution_count": "2",
            "output_count": 1,
        }
    });

    write_scenario("re_execution", &mut daemon, &manifest);
}

#[test]
fn scenario_multi_cell_execution() {
    //! Multiple cells executed in sequence.

    let mut daemon = NotebookDoc::new_with_actor("multi-cell", "fixture-multi-cell");
    daemon.add_cell(0, "cell-1", "code").unwrap();
    daemon.update_source("cell-1", "x = 42").unwrap();
    daemon.add_cell(1, "cell-2", "code").unwrap();
    daemon.update_source("cell-2", "print(x)").unwrap();
    daemon.add_cell(2, "cell-3", "markdown").unwrap();
    daemon.update_source("cell-3", "# Results").unwrap();

    // Execute cell-1 (no output)
    daemon.set_execution_count("cell-1", "1").unwrap();

    // Execute cell-2 (stream output)
    daemon.set_execution_count("cell-2", "2").unwrap();
    daemon
        .append_output(
            "cell-2",
            &json!({"output_type": "stream", "name": "stdout", "text": "42\n"}).to_string(),
        )
        .unwrap();

    let manifest = json!({
        "scenario": "multi_cell_execution",
        "description": "Two code cells + markdown, sequential execution",
        "expected": {
            "cell_count": 3,
            "cells": [
                {"cell_id": "cell-1", "execution_count": "1", "output_count": 0},
                {"cell_id": "cell-2", "execution_count": "2", "output_count": 1},
                {"cell_id": "cell-3", "cell_type": "markdown", "source": "# Results"},
            ]
        }
    });

    write_scenario("multi_cell_execution", &mut daemon, &manifest);
}

#[test]
fn scenario_display_data_output() {
    //! Cell produces display_data with a manifest hash (simulated blob store).

    let mut daemon = NotebookDoc::new_with_actor("display-data", "fixture-display-data");
    daemon.add_cell(0, "cell-1", "code").unwrap();
    daemon
        .update_source(
            "cell-1",
            "import matplotlib.pyplot as plt\nplt.plot([1,2,3])\nplt.show()",
        )
        .unwrap();

    daemon.set_execution_count("cell-1", "1").unwrap();
    let manifest_hash = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
    daemon.append_output("cell-1", manifest_hash).unwrap();

    let manifest = json!({
        "scenario": "display_data_output",
        "description": "Cell produces display_data with manifest hash (simulated image)",
        "expected": {
            "cell_id": "cell-1",
            "execution_count": "1",
            "output_count": 1,
            "output_is_manifest_hash": true,
        }
    });

    write_scenario("display_data_output", &mut daemon, &manifest);
}
