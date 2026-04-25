//! Conversion layer between the daemon's runtime types and `nbformat::v4::*`.
//!
//! Our runtime shape (`OutputManifest`, `CellSnapshot`, room metadata) is
//! tuned for the daemon's needs — blob-store offload via `ContentRef`,
//! `output_id` for in-memory indexing, LLM preview fields, Automerge-friendly
//! string encodings. None of that belongs on disk in an `.ipynb`.
//!
//! This module is the boundary where we translate into the typed nbformat
//! schema. Once a value is a `v4::Notebook`, `nbformat::serialize_notebook`
//! handles sort-keys, 1-space indent, trailing newline, and the structural
//! invariants (cell-id presence, output typing) that we previously enforced
//! by hand in `persist.rs`.
//!
//! All conversions are **save-only**. Loads still go through `load.rs` which
//! resolves into `OutputManifest`/`CellSnapshot` directly.
//!
//! Design notes:
//!
//! - `OutputManifest -> v4::Output` uses the nbformat/jupyter-protocol
//!   deserializers. We resolve every `ContentRef` to its final string/Value
//!   form, build the Jupyter-shape map, then `serde_json::from_value` into
//!   the typed enum. The crate handles MIME dispatch (`MediaType::Png`,
//!   `::Plain`, `::Other(...)` fallback for unknowns).
//! - Runtime-only fields (`output_id`, `llm_preview`, `rich`, `transient`)
//!   are dropped at this boundary by construction — nbformat's structs don't
//!   have them.
//! - Errors surfaces: if an output can't be converted (blob missing, invalid
//!   JSON, impossible MIME bundle), we return a fallback stream output
//!   pointing at the failure so the save succeeds. Losing an output silently
//!   is worse than writing a visible marker.

use std::collections::HashMap;

use nbformat::v4::{Cell, CellId, CellMetadata, Metadata, MultilineString, Notebook, Output};
use serde_json::{json, Value};

use notebook_doc::CellSnapshot;

/// Build a valid `v4::Notebook` from the daemon's in-memory pieces.
///
/// Inputs:
/// - `cells`: cell snapshots from the Automerge doc, in display order.
/// - `cell_outputs`: resolved-but-still-JSON outputs, keyed by cell id. Each
///   Value must have an `output_type` field (the daemon writes these shapes
///   from `resolve_manifest`).
/// - `cell_execution_counts`: per-cell execution_count override, keyed by
///   cell id. Takes precedence over whatever the snapshot carried.
/// - `cell_attachments`: markdown/raw nbformat attachments, keyed by cell id.
/// - `metadata_value`: the merged metadata JSON (existing on-disk metadata +
///   runtime snapshot overlay). Deserialized into `v4::Metadata`.
/// - `nbformat_minor`: at least 5 (we always write cell ids).
pub(crate) fn build_v4_notebook(
    cells: &[CellSnapshot],
    cell_outputs: &HashMap<String, Vec<Value>>,
    cell_execution_counts: &HashMap<String, Option<i64>>,
    cell_attachments: &HashMap<String, Value>,
    metadata_value: &Value,
    nbformat_minor: i32,
) -> Result<Notebook, NbformatConvertError> {
    let metadata = metadata_value_to_v4(metadata_value)?;

    let mut v4_cells = Vec::with_capacity(cells.len());
    for cell in cells {
        let converted = cell_to_v4(
            cell,
            cell_outputs.get(&cell.id).map(Vec::as_slice).unwrap_or(&[]),
            cell_execution_counts.get(&cell.id).copied().flatten(),
            cell_attachments.get(&cell.id),
        )?;
        v4_cells.push(converted);
    }

    Ok(Notebook {
        metadata,
        nbformat: 4,
        nbformat_minor,
        cells: v4_cells,
    })
}

/// Convert a single `CellSnapshot` plus resolved outputs into `v4::Cell`.
fn cell_to_v4(
    cell: &CellSnapshot,
    outputs: &[Value],
    execution_count_override: Option<i64>,
    attachments: Option<&Value>,
) -> Result<Cell, NbformatConvertError> {
    let id = CellId::new(&cell.id).map_err(|_| NbformatConvertError::InvalidCellId {
        id: cell.id.clone(),
    })?;
    let metadata = cell_metadata_value_to_v4(&cell.metadata)?;
    let source = split_source_lines(&cell.source);

    match cell.cell_type.as_str() {
        "code" => {
            let execution_count = execution_count_override
                .map(|n| n as i32)
                .or_else(|| parse_execution_count_field(&cell.execution_count));
            let v4_outputs = outputs
                .iter()
                .map(output_value_to_v4)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Cell::Code {
                id,
                metadata,
                execution_count,
                source,
                outputs: v4_outputs,
            })
        }
        "markdown" => Ok(Cell::Markdown {
            id,
            metadata,
            source,
            attachments: attachments.cloned(),
        }),
        "raw" => {
            // Known limitation: nbformat 2.1.0's v4::Cell::Raw does not carry
            // an `attachments` field, even though the Jupyter v4.5 schema
            // permits it. If the source cell had attachments we log and drop
            // them rather than reshuffling into metadata (which would mislead
            // the reader). Tracked upstream; see runtimed/runtimed.
            if attachments.is_some() {
                tracing::warn!(
                    "[nbformat-convert] Raw cell '{}' has attachments; \
                     nbformat v4 Raw variant lacks an attachments field, dropping on save.",
                    cell.id
                );
            }
            Ok(Cell::Raw {
                id,
                metadata,
                source,
            })
        }
        other => Err(NbformatConvertError::UnknownCellType {
            cell_id: cell.id.clone(),
            cell_type: other.to_string(),
        }),
    }
}

/// Convert a resolved output `Value` (Jupyter-shaped JSON from
/// `resolve_manifest`) into a typed `v4::Output`.
///
/// Strategy: strip runtime-only fields, hand the trimmed value to nbformat's
/// deserializer. The typed enum gives us structural validation for free —
/// any shape it can't accept signals either a bug in `resolve_manifest` or a
/// genuinely malformed output from the kernel.
///
/// Fallback: on conversion failure we emit a stderr stream output with the
/// failure reason rather than dropping the output. The daemon's autosave
/// runs in the background; a silent drop would lose kernel output without
/// any signal.
fn output_value_to_v4(output: &Value) -> Result<Output, NbformatConvertError> {
    let mut trimmed = output.clone();
    strip_runtime_only_fields(&mut trimmed);

    match serde_json::from_value::<Output>(trimmed.clone()) {
        Ok(converted) => Ok(converted),
        Err(e) => {
            tracing::warn!(
                "[nbformat-convert] Failed to convert output to v4::Output ({e}); \
                 falling back to stderr marker. Raw output: {}",
                trimmed
            );
            Ok(Output::Stream {
                name: "stderr".to_string(),
                text: MultilineString(format!("[runtimed: output could not be serialized: {e}]\n")),
            })
        }
    }
}

/// Drop daemon-runtime fields that should never hit disk. These aren't part
/// of the nbformat schema and nbformat's typed structs don't have slots for
/// them — but `serde_json::from_value` would fail on unknown fields if we
/// left them in, so we scrub them explicitly.
fn strip_runtime_only_fields(output: &mut Value) {
    let Some(obj) = output.as_object_mut() else {
        return;
    };
    obj.remove("output_id");
    obj.remove("llm_preview");
    obj.remove("rich");
    // `transient` is valid on the wire protocol but not in `.ipynb`.
    obj.remove("transient");
}

/// Parse a JSON-encoded execution_count field (as stored on `CellSnapshot`).
/// Returns None for `"null"`, `""`, or anything that doesn't parse as i32.
fn parse_execution_count_field(encoded: &str) -> Option<i32> {
    if encoded.is_empty() || encoded == "null" {
        return None;
    }
    serde_json::from_str::<i32>(encoded).ok()
}

/// Split a cell's source string into the multi-line array nbformat expects.
/// Each resulting string preserves its trailing `\n` (via `split_inclusive`);
/// the last line has no newline unless the original source ended in one.
fn split_source_lines(source: &str) -> Vec<String> {
    if source.is_empty() {
        return Vec::new();
    }
    source
        .split_inclusive('\n')
        .map(|s| s.to_string())
        .collect()
}

/// Deserialize the merged metadata `Value` into a typed `v4::Metadata`. The
/// metadata snapshot merger produces a standard Jupyter-shape object, so
/// this is a round-trip through serde.
fn metadata_value_to_v4(value: &Value) -> Result<Metadata, NbformatConvertError> {
    if value.is_null() {
        return Ok(Metadata::default());
    }
    serde_json::from_value::<Metadata>(value.clone())
        .map_err(|e| NbformatConvertError::InvalidMetadata(e.to_string()))
}

/// Deserialize a cell's metadata `Value` into `v4::CellMetadata`. Unknown
/// fields land in `CellMetadata::additional` via `#[serde(flatten)]`.
/// Null or empty-object inputs both produce an all-None `CellMetadata`.
fn cell_metadata_value_to_v4(value: &Value) -> Result<CellMetadata, NbformatConvertError> {
    let normalized = if value.is_null() { &json!({}) } else { value };
    serde_json::from_value::<CellMetadata>(normalized.clone())
        .map_err(|e| NbformatConvertError::InvalidCellMetadata(e.to_string()))
}

/// Error surface for the conversion layer. Every variant is a build-a-
/// `.ipynb` failure; the save-site maps each to the right user-visible
/// outcome (retryable vs. unrecoverable).
#[derive(Debug)]
pub(crate) enum NbformatConvertError {
    InvalidCellId { id: String },
    UnknownCellType { cell_id: String, cell_type: String },
    InvalidMetadata(String),
    InvalidCellMetadata(String),
}

impl std::fmt::Display for NbformatConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NbformatConvertError::InvalidCellId { id } => {
                write!(f, "cell id '{}' is not a valid nbformat CellId", id)
            }
            NbformatConvertError::UnknownCellType { cell_id, cell_type } => {
                write!(
                    f,
                    "cell '{cell_id}' has unsupported cell_type '{cell_type}'"
                )
            }
            NbformatConvertError::InvalidMetadata(msg) => {
                write!(f, "notebook metadata failed v4 validation: {msg}")
            }
            NbformatConvertError::InvalidCellMetadata(msg) => {
                write!(f, "cell metadata failed v4 validation: {msg}")
            }
        }
    }
}

impl std::error::Error for NbformatConvertError {}

#[cfg(test)]
mod tests {
    use super::*;
    use nbformat::v4::ErrorOutput;
    use serde_json::json;

    fn cell_snapshot(id: &str, cell_type: &str, source: &str) -> CellSnapshot {
        CellSnapshot {
            id: id.to_string(),
            cell_type: cell_type.to_string(),
            position: "80".to_string(),
            source: source.to_string(),
            execution_count: "null".to_string(),
            metadata: json!({}),
            resolved_assets: Default::default(),
        }
    }

    #[test]
    fn split_source_lines_preserves_trailing_newlines() {
        assert_eq!(split_source_lines(""), Vec::<String>::new());
        assert_eq!(split_source_lines("x = 1"), vec!["x = 1"]);
        assert_eq!(split_source_lines("x = 1\n"), vec!["x = 1\n"]);
        assert_eq!(
            split_source_lines("a = 1\nb = 2\n"),
            vec!["a = 1\n", "b = 2\n"]
        );
        assert_eq!(split_source_lines("a = 1\nb = 2"), vec!["a = 1\n", "b = 2"]);
    }

    #[test]
    fn parse_execution_count_handles_null_and_integers() {
        assert_eq!(parse_execution_count_field(""), None);
        assert_eq!(parse_execution_count_field("null"), None);
        assert_eq!(parse_execution_count_field("1"), Some(1));
        assert_eq!(parse_execution_count_field("42"), Some(42));
        assert_eq!(parse_execution_count_field("not-a-number"), None);
    }

    #[test]
    fn strip_runtime_only_fields_removes_daemon_bookkeeping() {
        let mut out = json!({
            "output_type": "stream",
            "output_id": "abc",
            "llm_preview": { "text": "..." },
            "rich": { "blob": "deadbeef", "size": 10 },
            "transient": { "display_id": "x" },
            "name": "stdout",
            "text": "hi\n",
        });
        strip_runtime_only_fields(&mut out);
        let obj = out.as_object().unwrap();
        assert!(!obj.contains_key("output_id"));
        assert!(!obj.contains_key("llm_preview"));
        assert!(!obj.contains_key("rich"));
        assert!(!obj.contains_key("transient"));
        assert!(obj.contains_key("output_type"));
        assert!(obj.contains_key("name"));
        assert!(obj.contains_key("text"));
    }

    #[test]
    fn output_value_to_v4_handles_stream_output() {
        let raw = json!({
            "output_type": "stream",
            "output_id": "abc",
            "name": "stdout",
            "text": "hello\n",
        });
        let v4 = output_value_to_v4(&raw).unwrap();
        match v4 {
            Output::Stream { name, text } => {
                assert_eq!(name, "stdout");
                assert_eq!(text.0, "hello\n");
            }
            other => panic!("expected Stream, got {other:?}"),
        }
    }

    #[test]
    fn output_value_to_v4_handles_execute_result() {
        let raw = json!({
            "output_type": "execute_result",
            "output_id": "abc",
            "execution_count": 3,
            "data": { "text/plain": "42" },
            "metadata": {},
        });
        let v4 = output_value_to_v4(&raw).unwrap();
        match v4 {
            Output::ExecuteResult(er) => {
                assert_eq!(er.execution_count.value(), 3);
            }
            other => panic!("expected ExecuteResult, got {other:?}"),
        }
    }

    #[test]
    fn output_value_to_v4_handles_error() {
        let raw = json!({
            "output_type": "error",
            "output_id": "abc",
            "ename": "NameError",
            "evalue": "name 'x' is not defined",
            "traceback": ["line 1", "line 2"],
        });
        let v4 = output_value_to_v4(&raw).unwrap();
        match v4 {
            Output::Error(ErrorOutput {
                ename,
                evalue,
                traceback,
            }) => {
                assert_eq!(ename, "NameError");
                assert_eq!(evalue, "name 'x' is not defined");
                assert_eq!(traceback, vec!["line 1", "line 2"]);
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn output_value_to_v4_falls_back_on_invalid_shape() {
        // Missing required fields — e.g. stream without name/text — should
        // surface as a stderr marker, not an unwrap crash.
        let raw = json!({
            "output_type": "stream",
        });
        let v4 = output_value_to_v4(&raw).unwrap();
        match v4 {
            Output::Stream { name, text } => {
                assert_eq!(name, "stderr");
                assert!(text.0.contains("could not be serialized"));
            }
            other => panic!("expected fallback Stream, got {other:?}"),
        }
    }

    #[test]
    fn cell_to_v4_builds_code_cell() {
        let snap = CellSnapshot {
            id: "cell-1".to_string(),
            cell_type: "code".to_string(),
            position: "80".to_string(),
            source: "print('hi')\n".to_string(),
            execution_count: "2".to_string(),
            metadata: json!({}),
            resolved_assets: Default::default(),
        };
        let outputs = vec![json!({
            "output_type": "stream",
            "output_id": "runtime-only",
            "name": "stdout",
            "text": "hi\n",
        })];
        let v4 = cell_to_v4(&snap, &outputs, None, None).unwrap();
        match v4 {
            Cell::Code {
                id,
                execution_count,
                source,
                outputs,
                ..
            } => {
                assert_eq!(id.as_str(), "cell-1");
                assert_eq!(execution_count, Some(2));
                assert_eq!(source, vec!["print('hi')\n"]);
                assert_eq!(outputs.len(), 1);
                assert!(matches!(outputs[0], Output::Stream { .. }));
            }
            other => panic!("expected Code, got {other:?}"),
        }
    }

    #[test]
    fn cell_to_v4_execution_count_override_takes_precedence() {
        let snap = cell_snapshot("cell-1", "code", "x = 1");
        // Snapshot says null, override says 5.
        let v4 = cell_to_v4(&snap, &[], Some(5), None).unwrap();
        match v4 {
            Cell::Code {
                execution_count, ..
            } => assert_eq!(execution_count, Some(5)),
            other => panic!("expected Code, got {other:?}"),
        }
    }

    #[test]
    fn cell_to_v4_builds_markdown_with_attachments() {
        let snap = cell_snapshot("md-1", "markdown", "# heading");
        let attachments = json!({ "image.png": { "image/png": "base64..." } });
        let v4 = cell_to_v4(&snap, &[], None, Some(&attachments)).unwrap();
        match v4 {
            Cell::Markdown {
                attachments: out, ..
            } => {
                assert_eq!(out, Some(attachments));
            }
            other => panic!("expected Markdown, got {other:?}"),
        }
    }

    #[test]
    fn cell_to_v4_rejects_unknown_cell_type() {
        let snap = cell_snapshot("cell-1", "quantum", "");
        let err = cell_to_v4(&snap, &[], None, None).unwrap_err();
        assert!(matches!(err, NbformatConvertError::UnknownCellType { .. }));
    }

    #[test]
    fn cell_to_v4_rejects_invalid_cell_id() {
        let snap = cell_snapshot("$ bad $", "code", "x = 1");
        let err = cell_to_v4(&snap, &[], None, None).unwrap_err();
        assert!(matches!(err, NbformatConvertError::InvalidCellId { .. }));
    }

    #[test]
    fn build_v4_notebook_full_round_trip() {
        let cells = vec![
            CellSnapshot {
                id: "cell-1".to_string(),
                cell_type: "code".to_string(),
                position: "80".to_string(),
                source: "x = 1".to_string(),
                execution_count: "1".to_string(),
                metadata: json!({"tags": ["demo"]}),
                resolved_assets: Default::default(),
            },
            cell_snapshot("cell-2", "markdown", "# heading"),
        ];
        let outputs: HashMap<String, Vec<Value>> = [(
            "cell-1".to_string(),
            vec![json!({
                "output_type": "stream",
                "output_id": "runtime-only",
                "name": "stdout",
                "text": "ok\n",
            })],
        )]
        .into_iter()
        .collect();
        let exec_counts: HashMap<String, Option<i64>> = HashMap::new();
        let attachments: HashMap<String, Value> = HashMap::new();
        let metadata = json!({
            "kernelspec": {
                "name": "python3",
                "display_name": "Python 3",
                "language": "python",
            },
        });

        let nb = build_v4_notebook(&cells, &outputs, &exec_counts, &attachments, &metadata, 5)
            .expect("build");
        assert_eq!(nb.nbformat, 4);
        assert_eq!(nb.nbformat_minor, 5);
        assert_eq!(nb.cells.len(), 2);
        assert!(nb.metadata.kernelspec.is_some());

        // Serialize and re-parse to confirm it's valid nbformat.
        let serialized =
            nbformat::serialize_notebook(&nbformat::Notebook::V4(nb)).expect("serialize_notebook");
        let parsed = nbformat::parse_notebook(&serialized).expect("parse");
        match parsed {
            nbformat::Notebook::V4(v4) => {
                assert_eq!(v4.cells.len(), 2);
                // Runtime-only output_id must not survive the round-trip.
                assert!(!serialized.contains("output_id"));
                // Top-level keys alphabetical per nbformat 2.1.0.
                let c = serialized.find("\"cells\"").unwrap();
                let m = serialized.find("\"metadata\"").unwrap();
                let nf = serialized.find("\"nbformat\"").unwrap();
                let mi = serialized.find("\"nbformat_minor\"").unwrap();
                assert!(c < m && m < nf && nf < mi);
            }
            other => panic!("expected V4 notebook, got {other:?}"),
        }
    }
}
