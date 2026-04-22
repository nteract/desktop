#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Generate Automerge document fixtures for frontend (vitest) integration tests.
//!
//! Each scenario creates a NotebookDoc with daemon-authored mutations (outputs,
//! execution counts, etc.) and saves the full doc to `packages/runtimed/tests/fixtures/`.
//!
//! Outputs mirror the production shape exactly: inline manifest objects are
//! written into `RuntimeStateDoc.executions.{eid}.outputs` via `set_outputs`.
//! Every output carries a deterministic `output_id` (UUIDv5 derived from the
//! scenario + execution_id + index) so regenerating fixtures does not churn.
//!
//! The frontend tests load these docs into a WASM "server" handle, then use
//! DirectTransport to sync to a fresh WASM client handle — driving the real
//! 2-party Automerge sync protocol through the SyncEngine pipeline.
//!
//! Run with:
//!   cargo test -p notebook-doc --test generate_fixtures -- --nocapture

use automerge::transaction::Transactable;
use notebook_doc::runtime_state::RuntimeStateDoc;
use notebook_doc::{frame_types, NotebookDoc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

/// Write execution_count directly via raw Automerge put.
/// `NotebookDoc::set_execution_count` was removed — RuntimeStateDoc is
/// the source of truth. This helper is only for fixture generation.
fn set_execution_count_raw(doc: &mut NotebookDoc, cell_id: &str, count: &str) {
    let cell_obj = doc.cell_obj_for(cell_id).expect("cell not found");
    doc.doc_mut()
        .put(&cell_obj, "execution_count", count)
        .expect("put execution_count");
}

// ── Manifest types (mirrors runtimed::output_store wire shape) ──────

/// Content reference: inlined for small data, blob hash for large.
/// Fixtures inline everything except a single display_data image, where a
/// blob ref exercises the non-inline path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum ContentRef {
    Inline {
        inline: String,
    },
    #[allow(dead_code)]
    Blob {
        blob: String,
        size: u64,
    },
}

/// Output manifest — the JSON shape stored inline in `ExecutionState.outputs`.
/// Matches `runtimed::output_store::OutputManifest` except for the optional
/// metadata/buffer fields the frontend does not inspect.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "output_type")]
enum OutputManifest {
    #[serde(rename = "stream")]
    Stream {
        output_id: String,
        name: String,
        text: ContentRef,
    },
    #[serde(rename = "error")]
    Error {
        output_id: String,
        ename: String,
        evalue: String,
        traceback: ContentRef,
    },
    #[serde(rename = "execute_result")]
    ExecuteResult {
        output_id: String,
        data: BTreeMap<String, ContentRef>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        metadata: BTreeMap<String, Value>,
        execution_count: Option<i32>,
    },
    #[serde(rename = "display_data")]
    DisplayData {
        output_id: String,
        data: BTreeMap<String, ContentRef>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        metadata: BTreeMap<String, Value>,
    },
}

impl OutputManifest {
    fn output_id(&self) -> &str {
        match self {
            OutputManifest::Stream { output_id, .. }
            | OutputManifest::Error { output_id, .. }
            | OutputManifest::ExecuteResult { output_id, .. }
            | OutputManifest::DisplayData { output_id, .. } => output_id,
        }
    }

    fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("manifest serialize")
    }
}

/// Deterministic output_id derived from (scenario, execution_id, index).
///
/// Production stamps a fresh UUIDv4 per output — for fixtures we want
/// stable ids across regenerations so the saved `doc.bin` stays
/// reproducible. UUIDv5 over a fixed namespace serves that purpose.
fn fixture_output_id(scenario: &str, execution_id: &str, index: usize) -> String {
    // Arbitrary but stable namespace for fixture ids.
    let ns = Uuid::parse_str("9f5f6d2d-0b6a-4a5e-9e5c-6a5b5e5a5b5e").unwrap();
    let name = format!("{scenario}:{execution_id}:{index}");
    Uuid::new_v5(&ns, name.as_bytes()).to_string()
}

fn inline(s: &str) -> ContentRef {
    ContentRef::Inline {
        inline: s.to_string(),
    }
}

// ── Fixture writing ─────────────────────────────────────────────────

/// Directory where fixtures are written.
fn fixtures_dir() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir).join("../../packages/runtimed/tests/fixtures")
}

/// Frame: [type_byte, ...json_bytes]
fn make_broadcast_frame(broadcast: &Value) -> Vec<u8> {
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

/// Write a scenario: manifest.json + doc.bin + state_doc.bin.
///
/// No blobs/ directory — outputs live inline in `state_doc.bin` and can
/// be read directly from the WASM handle.
fn write_scenario(
    name: &str,
    daemon: &mut NotebookDoc,
    state_doc: &mut RuntimeStateDoc,
    test_manifest: &Value,
) {
    let dir = clean_scenario_dir(name);
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(test_manifest).unwrap(),
    )
    .unwrap();
    fs::write(dir.join("doc.bin"), daemon.save()).unwrap();
    fs::write(dir.join("state_doc.bin"), state_doc.doc_mut().save()).unwrap();
}

/// Write a scenario with broadcast frame files alongside the doc.
fn write_scenario_with_broadcasts(
    name: &str,
    daemon: &mut NotebookDoc,
    state_doc: &mut RuntimeStateDoc,
    test_manifest: &Value,
    broadcast_frames: &[Vec<u8>],
) {
    write_scenario(name, daemon, state_doc, test_manifest);
    let dir = fixtures_dir().join(name);
    for (i, frame) in broadcast_frames.iter().enumerate() {
        fs::write(dir.join(format!("broadcast_{i:03}.bin")), frame).unwrap();
    }
}

/// Attach a list of inline manifest objects to an execution and link the
/// cell's `execution_id` pointer to it.
fn fixture_add_execution(
    doc: &mut NotebookDoc,
    state_doc: &mut RuntimeStateDoc,
    cell_id: &str,
    execution_id: &str,
    manifests: &[OutputManifest],
) {
    let _ = state_doc.create_execution(execution_id, cell_id);
    let _ = state_doc.set_execution_done(execution_id, true);
    if !manifests.is_empty() {
        let values: Vec<Value> = manifests.iter().map(OutputManifest::to_value).collect();
        state_doc
            .set_outputs(execution_id, &values)
            .expect("set_outputs");
    }
    doc.set_execution_id(cell_id, Some(execution_id))
        .expect("set_execution_id");
}

/// Collect `output_ids` in emission order for use in the scenario manifest.
fn output_ids(manifests: &[OutputManifest]) -> Vec<&str> {
    manifests.iter().map(OutputManifest::output_id).collect()
}

// ── Scenarios ────────────────────────────────────────────────────────

#[test]
fn scenario_output_streaming() {
    //! Daemon creates a cell, executes it, and streams stdout output.

    let scenario = "output_streaming";
    let mut daemon = NotebookDoc::new_with_actor("output-streaming", "fixture-output-streaming");
    let mut state_doc = RuntimeStateDoc::new();
    daemon.add_cell(0, "cell-1", "code").unwrap();
    daemon
        .update_source("cell-1", "for i in range(3):\n    print(i)")
        .unwrap();

    set_execution_count_raw(&mut daemon, "cell-1", "1");

    let lines = ["0\n", "1\n", "2\n"];
    let manifests: Vec<OutputManifest> = lines
        .iter()
        .enumerate()
        .map(|(i, line)| OutputManifest::Stream {
            output_id: fixture_output_id(scenario, "exec-001", i),
            name: "stdout".to_string(),
            text: inline(line),
        })
        .collect();

    fixture_add_execution(
        &mut daemon,
        &mut state_doc,
        "cell-1",
        "exec-001",
        &manifests,
    );

    // Broadcast frames carry the inline manifest JSON for each output.
    let broadcast_frames: Vec<Vec<u8>> = manifests
        .iter()
        .enumerate()
        .map(|(i, manifest)| {
            make_broadcast_frame(&json!({
                "event": "output",
                "cell_id": "cell-1",
                "execution_id": "exec-001",
                "output_type": "stream",
                "output_json": serde_json::to_string(manifest).unwrap(),
                "output_index": i,
            }))
        })
        .collect();

    let test_manifest = json!({
        "scenario": scenario,
        "description": "Daemon creates cell, executes, streams 3 stdout lines as inline manifests",
        "expected": {
            "cell_id": "cell-1",
            "source": "for i in range(3):\n    print(i)",
            "execution_count": "1",
            "output_count": 3,
            "output_ids": output_ids(&manifests),
            "output_texts": lines,
        }
    });

    write_scenario_with_broadcasts(
        scenario,
        &mut daemon,
        &mut state_doc,
        &test_manifest,
        &broadcast_frames,
    );
}

#[test]
fn scenario_execution_with_error() {
    //! Daemon executes a cell that raises an error.

    let scenario = "execution_with_error";
    let mut daemon = NotebookDoc::new_with_actor("error-execution", "fixture-error-execution");
    let mut state_doc = RuntimeStateDoc::new();
    daemon.add_cell(0, "cell-1", "code").unwrap();
    daemon.update_source("cell-1", "1 / 0").unwrap();

    set_execution_count_raw(&mut daemon, "cell-1", "1");

    let traceback = vec![
        "\u{001b}[0;31m---------------------------------------------------------------------------\u{001b}[0m",
        "\u{001b}[0;31mZeroDivisionError\u{001b}[0m: division by zero",
    ];
    let traceback_json = serde_json::to_string(&traceback).unwrap();

    let manifests = vec![OutputManifest::Error {
        output_id: fixture_output_id(scenario, "exec-001", 0),
        ename: "ZeroDivisionError".to_string(),
        evalue: "division by zero".to_string(),
        traceback: inline(&traceback_json),
    }];

    fixture_add_execution(
        &mut daemon,
        &mut state_doc,
        "cell-1",
        "exec-001",
        &manifests,
    );

    let test_manifest = json!({
        "scenario": scenario,
        "description": "Daemon executes cell that raises ZeroDivisionError",
        "expected": {
            "cell_id": "cell-1",
            "source": "1 / 0",
            "execution_count": "1",
            "output_count": 1,
            "output_ids": output_ids(&manifests),
        }
    });

    write_scenario(scenario, &mut daemon, &mut state_doc, &test_manifest);
}

#[test]
fn scenario_re_execution() {
    //! Cell executed twice. First outputs cleared, then new output written.

    let scenario = "re_execution";
    let mut daemon = NotebookDoc::new_with_actor("re-execution", "fixture-re-execution");
    let mut state_doc = RuntimeStateDoc::new();
    daemon.add_cell(0, "cell-1", "code").unwrap();
    daemon.update_source("cell-1", "print('hello')").unwrap();

    // First execution
    set_execution_count_raw(&mut daemon, "cell-1", "1");
    let first = vec![OutputManifest::Stream {
        output_id: fixture_output_id(scenario, "exec-001", 0),
        name: "stdout".to_string(),
        text: inline("hello\n"),
    }];
    fixture_add_execution(&mut daemon, &mut state_doc, "cell-1", "exec-001", &first);

    // Second execution: new execution_id implicitly replaces the first
    set_execution_count_raw(&mut daemon, "cell-1", "2");
    let mut data = BTreeMap::new();
    data.insert("text/plain".to_string(), inline("42"));
    let second = vec![OutputManifest::ExecuteResult {
        output_id: fixture_output_id(scenario, "exec-002", 0),
        data,
        metadata: BTreeMap::new(),
        execution_count: Some(2),
    }];
    fixture_add_execution(&mut daemon, &mut state_doc, "cell-1", "exec-002", &second);

    let test_manifest = json!({
        "scenario": scenario,
        "description": "Cell executed twice — only second execution's outputs remain",
        "expected": {
            "cell_id": "cell-1",
            "execution_count": "2",
            "output_count": 1,
            "output_ids": output_ids(&second),
        }
    });

    write_scenario(scenario, &mut daemon, &mut state_doc, &test_manifest);
}

#[test]
fn scenario_multi_cell_execution() {
    //! Multiple cells executed in sequence.

    let scenario = "multi_cell_execution";
    let mut daemon = NotebookDoc::new_with_actor("multi-cell", "fixture-multi-cell");
    let mut state_doc = RuntimeStateDoc::new();
    daemon.add_cell(0, "cell-1", "code").unwrap();
    daemon.update_source("cell-1", "x = 42").unwrap();
    daemon.add_cell(1, "cell-2", "code").unwrap();
    daemon.update_source("cell-2", "print(x)").unwrap();
    daemon.add_cell(2, "cell-3", "markdown").unwrap();
    daemon.update_source("cell-3", "# Results").unwrap();

    // Execute cell-1 (no output)
    set_execution_count_raw(&mut daemon, "cell-1", "1");
    fixture_add_execution(&mut daemon, &mut state_doc, "cell-1", "exec-001", &[]);

    // Execute cell-2 (stream output)
    set_execution_count_raw(&mut daemon, "cell-2", "2");
    let cell2_outputs = vec![OutputManifest::Stream {
        output_id: fixture_output_id(scenario, "exec-002", 0),
        name: "stdout".to_string(),
        text: inline("42\n"),
    }];
    fixture_add_execution(
        &mut daemon,
        &mut state_doc,
        "cell-2",
        "exec-002",
        &cell2_outputs,
    );

    let test_manifest = json!({
        "scenario": scenario,
        "description": "Two code cells + markdown, sequential execution",
        "expected": {
            "cell_count": 3,
            "cells": [
                {"cell_id": "cell-1", "execution_count": "1", "output_count": 0},
                {
                    "cell_id": "cell-2",
                    "execution_count": "2",
                    "output_count": 1,
                    "output_ids": output_ids(&cell2_outputs),
                },
                {"cell_id": "cell-3", "cell_type": "markdown", "source": "# Results"},
            ]
        }
    });

    write_scenario(scenario, &mut daemon, &mut state_doc, &test_manifest);
}

#[test]
fn scenario_display_data_output() {
    //! Cell produces display_data with inline text and a blob-ref image.

    let scenario = "display_data_output";
    let mut daemon = NotebookDoc::new_with_actor("display-data", "fixture-display-data");
    let mut state_doc = RuntimeStateDoc::new();
    daemon.add_cell(0, "cell-1", "code").unwrap();
    daemon
        .update_source(
            "cell-1",
            "import matplotlib.pyplot as plt\nplt.plot([1,2,3])\nplt.show()",
        )
        .unwrap();

    set_execution_count_raw(&mut daemon, "cell-1", "1");

    // display_data with text/plain (inlined) and image/png (blob ref — the
    // test verifies the ContentRef shape, not blob resolution).
    let mut data = BTreeMap::new();
    data.insert("text/plain".to_string(), inline("<Figure size 640x480>"));
    data.insert(
        "image/png".to_string(),
        ContentRef::Blob {
            blob: "fake_image_blob_hash_for_fixture_testing_only_not_real".to_string(),
            size: 12345,
        },
    );
    let manifests = vec![OutputManifest::DisplayData {
        output_id: fixture_output_id(scenario, "exec-001", 0),
        data,
        metadata: BTreeMap::new(),
    }];
    fixture_add_execution(
        &mut daemon,
        &mut state_doc,
        "cell-1",
        "exec-001",
        &manifests,
    );

    let test_manifest = json!({
        "scenario": scenario,
        "description": "Cell produces display_data with inline text and a blob-ref image",
        "expected": {
            "cell_id": "cell-1",
            "execution_count": "1",
            "output_count": 1,
            "output_ids": output_ids(&manifests),
        }
    });

    write_scenario(scenario, &mut daemon, &mut state_doc, &test_manifest);
}
