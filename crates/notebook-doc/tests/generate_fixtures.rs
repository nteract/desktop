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

use notebook_doc::{frame_types, NotebookDoc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

// ── Manifest types (mirrors runtimed::output_store) ─────────────────

/// Content reference: inlined for small data, blob hash for large.
/// Fixtures always inline since test outputs are tiny.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum ContentRef {
    Inline { inline: String },
    #[allow(dead_code)]
    Blob { blob: String, size: u64 },
}

/// Output manifest — the JSON structure stored in the blob store.
/// The CRDT stores only the SHA-256 hash of this JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "output_type")]
enum OutputManifest {
    #[serde(rename = "stream")]
    Stream { name: String, text: ContentRef },
    #[serde(rename = "error")]
    Error {
        ename: String,
        evalue: String,
        traceback: ContentRef,
    },
    #[serde(rename = "execute_result")]
    ExecuteResult {
        data: HashMap<String, ContentRef>,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        metadata: HashMap<String, Value>,
        execution_count: Option<i32>,
    },
    #[serde(rename = "display_data")]
    DisplayData {
        data: HashMap<String, ContentRef>,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        metadata: HashMap<String, Value>,
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

/// Write a scenario: manifest.json + doc.bin + blob manifest files.
fn write_scenario(
    name: &str,
    daemon: &mut NotebookDoc,
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
}

/// Write a scenario with broadcast frame files alongside the doc.
fn write_scenario_with_broadcasts(
    name: &str,
    daemon: &mut NotebookDoc,
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

    daemon.clear_outputs("cell-1").unwrap();
    daemon.set_execution_count("cell-1", "1").unwrap();

    // Build real manifests for each streamed line
    let lines = ["0\n", "1\n", "2\n"];
    let mut output_manifests = Vec::new();

    for line in &lines {
        let manifest = OutputManifest::Stream {
            name: "stdout".to_string(),
            text: inline(line),
        };
        let (hash, json) = hash_manifest(&manifest);
        daemon.append_output("cell-1", &hash).unwrap();
        output_manifests.push((hash, json));
    }

    // Broadcast frames reference the manifest hashes
    let broadcast_frames: Vec<Vec<u8>> = output_manifests
        .iter()
        .enumerate()
        .map(|(i, (_hash, manifest_json))| {
            make_broadcast_frame(&json!({
                "type": "Output",
                "cell_id": "cell-1",
                "execution_id": "exec-001",
                "output_type": "stream",
                "output_json": manifest_json,
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
        &test_manifest,
        &output_manifests,
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

    let traceback = vec![
        "\u{001b}[0;31m---------------------------------------------------------------------------\u{001b}[0m",
        "\u{001b}[0;31mZeroDivisionError\u{001b}[0m: division by zero",
    ];
    let traceback_json = serde_json::to_string(&traceback).unwrap();

    let manifest = OutputManifest::Error {
        ename: "ZeroDivisionError".to_string(),
        evalue: "division by zero".to_string(),
        traceback: inline(&traceback_json),
    };
    let (hash, manifest_json) = hash_manifest(&manifest);
    daemon.append_output("cell-1", &hash).unwrap();

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
        &test_manifest,
        &[(hash, manifest_json)],
    );
}

#[test]
fn scenario_re_execution() {
    //! Cell executed twice. First outputs cleared, then new output written.

    let mut daemon = NotebookDoc::new_with_actor("re-execution", "fixture-re-execution");
    daemon.add_cell(0, "cell-1", "code").unwrap();
    daemon.update_source("cell-1", "print('hello')").unwrap();

    // First execution
    daemon.set_execution_count("cell-1", "1").unwrap();
    let first_manifest = OutputManifest::Stream {
        name: "stdout".to_string(),
        text: inline("hello\n"),
    };
    let (first_hash, _) = hash_manifest(&first_manifest);
    daemon.append_output("cell-1", &first_hash).unwrap();

    // Second execution: clear then new output
    daemon.clear_outputs("cell-1").unwrap();
    daemon.set_execution_count("cell-1", "2").unwrap();

    let mut data = HashMap::new();
    data.insert("text/plain".to_string(), inline("42"));
    let second_manifest = OutputManifest::ExecuteResult {
        data,
        metadata: HashMap::new(),
        execution_count: Some(2),
    };
    let (second_hash, second_json) = hash_manifest(&second_manifest);
    daemon.append_output("cell-1", &second_hash).unwrap();

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
        &test_manifest,
        &[(second_hash, second_json)],
    );
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
    let manifest = OutputManifest::Stream {
        name: "stdout".to_string(),
        text: inline("42\n"),
    };
    let (hash, manifest_json) = hash_manifest(&manifest);
    daemon.append_output("cell-2", &hash).unwrap();

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
        &test_manifest,
        &[(hash, manifest_json)],
    );
}

#[test]
fn scenario_display_data_output() {
    //! Cell produces display_data with an image (manifest hash in CRDT).

    let mut daemon = NotebookDoc::new_with_actor("display-data", "fixture-display-data");
    daemon.add_cell(0, "cell-1", "code").unwrap();
    daemon
        .update_source(
            "cell-1",
            "import matplotlib.pyplot as plt\nplt.plot([1,2,3])\nplt.show()",
        )
        .unwrap();

    daemon.set_execution_count("cell-1", "1").unwrap();

    // display_data with text/plain (inlined) and image/png (would be a blob
    // ref in production, but we use inline here since we don't have a real
    // blob store — the test verifies the hash protocol, not blob resolution)
    let mut data = HashMap::new();
    data.insert("text/plain".to_string(), inline("<Figure size 640x480>"));
    data.insert(
        "image/png".to_string(),
        ContentRef::Blob {
            blob: "fake_image_blob_hash_for_fixture_testing_only_not_real".to_string(),
            size: 12345,
        },
    );
    let manifest = OutputManifest::DisplayData {
        data,
        metadata: HashMap::new(),
    };
    let (hash, manifest_json) = hash_manifest(&manifest);
    daemon.append_output("cell-1", &hash).unwrap();

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
        &test_manifest,
        &[(hash, manifest_json)],
    );
}
