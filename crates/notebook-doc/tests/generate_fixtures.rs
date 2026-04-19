#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Generate Automerge document fixtures for frontend (vitest) integration tests.
//!
//! Each scenario creates a NotebookDoc with daemon-authored mutations (outputs,
//! execution counts, etc.) and saves the full doc to `packages/runtimed/tests/fixtures/`.
//!
//! Outputs follow the real protocol: small content is inlined in a manifest JSON
//! object, the manifest is SHA-256 hashed, and only the hash is stored in the
//! CRDT. Manifest files are saved alongside the doc so tests can verify content
//! by resolving hashes to manifests.
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
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// Write execution_count directly via raw Automerge put.
/// `NotebookDoc::set_execution_count` was removed — RuntimeStateDoc is
/// the source of truth. This helper is only for fixture generation.
fn set_execution_count_raw(doc: &mut NotebookDoc, cell_id: &str, count: &str) {
    let cell_obj = doc.cell_obj_for(cell_id).expect("cell not found");
    doc.doc_mut()
        .put(&cell_obj, "execution_count", count)
        .expect("put execution_count");
}

// ── Manifest types (mirrors runtimed::output_store) ─────────────────

/// Content reference: inlined for small data, blob hash for large.
/// Fixtures always inline since test outputs are tiny.
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

/// Output manifest — the JSON structure stored in the blob store.
/// The CRDT stores only the SHA-256 hash of this JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "output_type")]
enum OutputManifest {
    #[serde(rename = "stream")]
    Stream {
        #[serde(default)]
        output_id: String,
        name: String,
        text: ContentRef,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(default)]
        output_id: String,
        ename: String,
        evalue: String,
        traceback: ContentRef,
    },
    #[serde(rename = "execute_result")]
    ExecuteResult {
        #[serde(default)]
        output_id: String,
        data: BTreeMap<String, ContentRef>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        metadata: BTreeMap<String, Value>,
        execution_count: Option<i32>,
    },
    #[serde(rename = "display_data")]
    DisplayData {
        #[serde(default)]
        output_id: String,
        data: BTreeMap<String, ContentRef>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        metadata: BTreeMap<String, Value>,
    },
}

/// Serialize a manifest to JSON and return its SHA-256 hash + the JSON bytes.
fn hash_manifest(manifest: &OutputManifest) -> (String, String) {
    let json = serde_json::to_string(manifest).unwrap();
    let hash = hex::encode(Sha256::digest(json.as_bytes()));
    (hash, json)
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

/// Store a manifest JSON file in the fixture's blobs/ directory, named by hash.
fn store_manifest_file(dir: &std::path::Path, hash: &str, manifest_json: &str) {
    let blobs_dir = dir.join("blobs");
    fs::create_dir_all(&blobs_dir).unwrap();
    fs::write(blobs_dir.join(hash), manifest_json).unwrap();
}

/// Write a scenario: manifest.json + doc.bin + state_doc.bin + blob manifest files.
fn write_scenario(
    name: &str,
    daemon: &mut NotebookDoc,
    state_doc: &mut RuntimeStateDoc,
    test_manifest: &serde_json::Value,
    output_manifests: &[(String, String)], // (hash, json) pairs
) {
    let dir = clean_scenario_dir(name);
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(test_manifest).unwrap(),
    )
    .unwrap();
    for (hash, json) in output_manifests {
        store_manifest_file(&dir, hash, json);
    }
    fs::write(dir.join("doc.bin"), daemon.save()).unwrap();
    fs::write(dir.join("state_doc.bin"), state_doc.doc_mut().save()).unwrap();
}

/// Write a scenario with broadcast frame files alongside the doc.
fn write_scenario_with_broadcasts(
    name: &str,
    daemon: &mut NotebookDoc,
    state_doc: &mut RuntimeStateDoc,
    test_manifest: &serde_json::Value,
    output_manifests: &[(String, String)],
    broadcast_frames: &[Vec<u8>],
) {
    let dir = clean_scenario_dir(name);
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(test_manifest).unwrap(),
    )
    .unwrap();
    for (hash, json) in output_manifests {
        store_manifest_file(&dir, hash, json);
    }
    fs::write(dir.join("doc.bin"), daemon.save()).unwrap();
    fs::write(dir.join("state_doc.bin"), state_doc.doc_mut().save()).unwrap();
    for (i, frame) in broadcast_frames.iter().enumerate() {
        fs::write(dir.join(format!("broadcast_{i:03}.bin")), frame).unwrap();
    }
}

/// Helper to add outputs to a RuntimeStateDoc for a cell.
///
/// Creates a synthetic execution entry, writes output hashes, and sets
/// the execution_id on the cell in the notebook doc.
fn fixture_add_outputs(
    doc: &mut NotebookDoc,
    state_doc: &mut RuntimeStateDoc,
    cell_id: &str,
    execution_id: &str,
    hashes: &[String],
) {
    // Write outputs to RuntimeStateDoc
    state_doc.create_execution(execution_id, cell_id);
    state_doc.set_execution_done(execution_id, true);
    if !hashes.is_empty() {
        // TODO(inline-manifests): fixture generator still uses hash strings.
        // Wrap each hash as a JSON string value so the new set_outputs API
        // compiles. This fixture generator needs a full rewrite to produce
        // inline manifest objects instead of hash refs.
        let manifests: Vec<serde_json::Value> = hashes
            .iter()
            .map(|h| serde_json::Value::String(h.clone()))
            .collect();
        state_doc
            .set_outputs(execution_id, &manifests)
            .expect("set_outputs");
    }
    // Link cell to execution_id in notebook doc
    doc.set_execution_id(cell_id, Some(execution_id))
        .expect("set_execution_id");
}

// ── Scenarios ────────────────────────────────────────────────────────

#[test]
fn scenario_output_streaming() {
    //! Daemon creates a cell, executes it, and streams stdout output.

    let mut daemon = NotebookDoc::new_with_actor("output-streaming", "fixture-output-streaming");
    let mut state_doc = RuntimeStateDoc::new();
    daemon.add_cell(0, "cell-1", "code").unwrap();
    daemon
        .update_source("cell-1", "for i in range(3):\n    print(i)")
        .unwrap();

    set_execution_count_raw(&mut daemon, "cell-1", "1");

    // Build real manifests for each streamed line
    let lines = ["0\n", "1\n", "2\n"];
    let mut output_manifests = Vec::new();
    let mut output_hashes = Vec::new();

    for line in &lines {
        let manifest = OutputManifest::Stream {
            output_id: String::new(),
            name: "stdout".to_string(),
            text: inline(line),
        };
        let (hash, json) = hash_manifest(&manifest);
        output_hashes.push(hash.clone());
        output_manifests.push((hash, json));
    }

    // Write outputs to RuntimeStateDoc
    fixture_add_outputs(
        &mut daemon,
        &mut state_doc,
        "cell-1",
        "exec-001",
        &output_hashes,
    );

    // Broadcast frames reference the manifest hashes
    let broadcast_frames: Vec<Vec<u8>> = output_manifests
        .iter()
        .enumerate()
        .map(|(i, (hash, _manifest_json))| {
            make_broadcast_frame(&json!({
                "event": "output",
                "cell_id": "cell-1",
                "execution_id": "exec-001",
                "output_type": "stream",
                "output_json": hash,
                "output_index": i,
            }))
        })
        .collect();

    let hashes: Vec<&str> = output_manifests.iter().map(|(h, _)| h.as_str()).collect();
    let test_manifest = json!({
        "scenario": "output_streaming",
        "description": "Daemon creates cell, executes, streams 3 stdout lines via manifest hashes",
        "expected": {
            "cell_id": "cell-1",
            "source": "for i in range(3):\n    print(i)",
            "execution_count": "1",
            "output_count": 3,
            "output_hashes": hashes,
        }
    });

    write_scenario_with_broadcasts(
        "output_streaming",
        &mut daemon,
        &mut state_doc,
        &test_manifest,
        &output_manifests,
        &broadcast_frames,
    );
}

#[test]
fn scenario_execution_with_error() {
    //! Daemon executes a cell that raises an error.

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

    let manifest = OutputManifest::Error {
        output_id: String::new(),
        ename: "ZeroDivisionError".to_string(),
        evalue: "division by zero".to_string(),
        traceback: inline(&traceback_json),
    };
    let (hash, manifest_json) = hash_manifest(&manifest);
    fixture_add_outputs(
        &mut daemon,
        &mut state_doc,
        "cell-1",
        "exec-001",
        std::slice::from_ref(&hash),
    );

    let test_manifest = json!({
        "scenario": "execution_with_error",
        "description": "Daemon executes cell that raises ZeroDivisionError",
        "expected": {
            "cell_id": "cell-1",
            "source": "1 / 0",
            "execution_count": "1",
            "output_count": 1,
            "output_hashes": [hash],
        }
    });

    write_scenario(
        "execution_with_error",
        &mut daemon,
        &mut state_doc,
        &test_manifest,
        &[(hash, manifest_json)],
    );
}

#[test]
fn scenario_re_execution() {
    //! Cell executed twice. First outputs cleared, then new output written.

    let mut daemon = NotebookDoc::new_with_actor("re-execution", "fixture-re-execution");
    let mut state_doc = RuntimeStateDoc::new();
    daemon.add_cell(0, "cell-1", "code").unwrap();
    daemon.update_source("cell-1", "print('hello')").unwrap();

    // First execution
    set_execution_count_raw(&mut daemon, "cell-1", "1");
    let first_manifest = OutputManifest::Stream {
        output_id: String::new(),
        name: "stdout".to_string(),
        text: inline("hello\n"),
    };
    let (first_hash, _) = hash_manifest(&first_manifest);
    fixture_add_outputs(
        &mut daemon,
        &mut state_doc,
        "cell-1",
        "exec-001",
        &[first_hash],
    );

    // Second execution: new execution_id implicitly replaces the first
    set_execution_count_raw(&mut daemon, "cell-1", "2");

    let mut data = BTreeMap::new();
    data.insert("text/plain".to_string(), inline("42"));
    let second_manifest = OutputManifest::ExecuteResult {
        output_id: String::new(),
        data,
        metadata: BTreeMap::new(),
        execution_count: Some(2),
    };
    let (second_hash, second_json) = hash_manifest(&second_manifest);
    fixture_add_outputs(
        &mut daemon,
        &mut state_doc,
        "cell-1",
        "exec-002",
        std::slice::from_ref(&second_hash),
    );

    let test_manifest = json!({
        "scenario": "re_execution",
        "description": "Cell executed twice — only second execution's outputs remain",
        "expected": {
            "cell_id": "cell-1",
            "execution_count": "2",
            "output_count": 1,
            "output_hashes": [second_hash],
        }
    });

    write_scenario(
        "re_execution",
        &mut daemon,
        &mut state_doc,
        &test_manifest,
        &[(second_hash, second_json)],
    );
}

#[test]
fn scenario_multi_cell_execution() {
    //! Multiple cells executed in sequence.

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
    fixture_add_outputs(&mut daemon, &mut state_doc, "cell-1", "exec-001", &[]);

    // Execute cell-2 (stream output)
    set_execution_count_raw(&mut daemon, "cell-2", "2");
    let manifest = OutputManifest::Stream {
        output_id: String::new(),
        name: "stdout".to_string(),
        text: inline("42\n"),
    };
    let (hash, manifest_json) = hash_manifest(&manifest);
    fixture_add_outputs(
        &mut daemon,
        &mut state_doc,
        "cell-2",
        "exec-002",
        std::slice::from_ref(&hash),
    );

    let test_manifest = json!({
        "scenario": "multi_cell_execution",
        "description": "Two code cells + markdown, sequential execution",
        "expected": {
            "cell_count": 3,
            "cells": [
                {"cell_id": "cell-1", "execution_count": "1", "output_count": 0},
                {"cell_id": "cell-2", "execution_count": "2", "output_count": 1, "output_hashes": [hash]},
                {"cell_id": "cell-3", "cell_type": "markdown", "source": "# Results"},
            ]
        }
    });

    write_scenario(
        "multi_cell_execution",
        &mut daemon,
        &mut state_doc,
        &test_manifest,
        &[(hash, manifest_json)],
    );
}

#[test]
fn scenario_display_data_output() {
    //! Cell produces display_data with an image (manifest hash in CRDT).

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

    // display_data with text/plain (inlined) and image/png (would be a blob
    // ref in production, but we use inline here since we don't have a real
    // blob store — the test verifies the hash protocol, not blob resolution)
    let mut data = BTreeMap::new();
    data.insert("text/plain".to_string(), inline("<Figure size 640x480>"));
    data.insert(
        "image/png".to_string(),
        ContentRef::Blob {
            blob: "fake_image_blob_hash_for_fixture_testing_only_not_real".to_string(),
            size: 12345,
        },
    );
    let manifest = OutputManifest::DisplayData {
        output_id: String::new(),
        data,
        metadata: BTreeMap::new(),
    };
    let (hash, manifest_json) = hash_manifest(&manifest);
    fixture_add_outputs(
        &mut daemon,
        &mut state_doc,
        "cell-1",
        "exec-001",
        std::slice::from_ref(&hash),
    );

    let test_manifest = json!({
        "scenario": "display_data_output",
        "description": "Cell produces display_data with manifest hash (text + image)",
        "expected": {
            "cell_id": "cell-1",
            "execution_count": "1",
            "output_count": 1,
            "output_hashes": [hash],
        }
    });

    write_scenario(
        "display_data_output",
        &mut daemon,
        &mut state_doc,
        &test_manifest,
        &[(hash, manifest_json)],
    );
}
