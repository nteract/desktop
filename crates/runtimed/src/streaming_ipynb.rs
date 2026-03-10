//! Streaming .ipynb parser using jiter for incremental notebook loading.
//!
//! This module provides a fast, streaming JSON parser for Jupyter notebook files.
//! Instead of loading the entire file into a serde_json::Value tree, it parses
//! cells incrementally and invokes a callback for each cell. This enables
//! streaming cells to the frontend via Automerge sync messages during load.

use jiter::{Jiter, JsonValue, Peek};
use notebook_doc::{metadata::NotebookMetadataSnapshot, CellSnapshot};
use thiserror::Error;

/// Errors that can occur during streaming notebook parsing.
#[derive(Debug, Error)]
pub enum StreamError {
    #[error("JSON parse error at byte {position}: {message}")]
    JsonParse { position: usize, message: String },

    #[error("Invalid notebook structure: {0}")]
    InvalidStructure(String),

    #[error("Cell processing error: {0}")]
    CellProcessing(String),
}

impl From<jiter::JsonError> for StreamError {
    fn from(e: jiter::JsonError) -> Self {
        StreamError::JsonParse {
            position: e.index,
            message: e.error_type.to_string(),
        }
    }
}

impl From<jiter::JiterError> for StreamError {
    fn from(e: jiter::JiterError) -> Self {
        StreamError::JsonParse {
            position: e.index,
            message: e.error_type.to_string(),
        }
    }
}

/// Result of streaming notebook parse.
pub struct StreamParseResult {
    pub metadata: NotebookMetadataSnapshot,
    pub cell_count: usize,
}

/// Parse a .ipynb file incrementally, calling `on_cell` for each cell.
///
/// This function uses jiter to parse the notebook JSON without building
/// a full in-memory tree. Cells are emitted one at a time via the callback,
/// allowing the caller to add them to the Automerge doc and send sync
/// messages incrementally.
///
/// Returns the notebook metadata and total cell count on success.
pub fn stream_notebook_cells<F>(
    data: &[u8],
    mut on_cell: F,
) -> Result<StreamParseResult, StreamError>
where
    F: FnMut(CellSnapshot, usize) -> Result<(), StreamError>,
{
    let mut jiter = Jiter::new(data);
    let mut metadata: Option<NotebookMetadataSnapshot> = None;
    let mut cell_count = 0;

    // next_object() returns Option<&str> for first key (or None if empty)
    // Convert to owned String to release the borrow before using jiter again
    let first_key = jiter.next_object()?.map(|s| s.to_string());

    if let Some(key) = first_key {
        process_notebook_key(
            &key,
            &mut jiter,
            &mut cell_count,
            &mut metadata,
            &mut on_cell,
        )?;

        // Process remaining keys
        loop {
            let next_key = jiter.next_key()?.map(|s| s.to_string());
            match next_key {
                Some(key) => {
                    process_notebook_key(
                        &key,
                        &mut jiter,
                        &mut cell_count,
                        &mut metadata,
                        &mut on_cell,
                    )?;
                }
                None => break,
            }
        }
    }

    Ok(StreamParseResult {
        metadata: metadata.unwrap_or_default(),
        cell_count,
    })
}

/// Process a single notebook-level key-value pair.
fn process_notebook_key<F>(
    key: &str,
    jiter: &mut Jiter,
    cell_count: &mut usize,
    metadata: &mut Option<NotebookMetadataSnapshot>,
    on_cell: &mut F,
) -> Result<(), StreamError>
where
    F: FnMut(CellSnapshot, usize) -> Result<(), StreamError>,
{
    match key {
        "cells" => {
            // Parse cells array incrementally
            // next_array() returns Option<Peek> for first element (or None if empty)
            if let Some(first_peek) = jiter.next_array()? {
                if first_peek == Peek::Object {
                    let cell = parse_cell(jiter, *cell_count)?;
                    on_cell(cell, *cell_count)?;
                    *cell_count += 1;

                    // Continue with remaining elements
                    while let Some(peek) = jiter.array_step()? {
                        if peek == Peek::Object {
                            let cell = parse_cell(jiter, *cell_count)?;
                            on_cell(cell, *cell_count)?;
                            *cell_count += 1;
                        } else {
                            jiter.next_skip()?;
                        }
                    }
                } else {
                    jiter.next_skip()?;
                    // Handle remaining elements
                    while jiter.array_step()?.is_some() {
                        jiter.next_skip()?;
                    }
                }
            }
        }
        "metadata" => {
            // Parse metadata as JSON value, then convert
            let metadata_value = jiter.next_value()?;
            *metadata = Some(parse_metadata_from_value(&metadata_value)?);
        }
        _ => {
            // Skip nbformat, nbformat_minor, etc.
            jiter.next_skip()?;
        }
    }
    Ok(())
}

/// Parse a single cell object from the jiter stream.
fn parse_cell(jiter: &mut Jiter, index: usize) -> Result<CellSnapshot, StreamError> {
    let mut id: Option<String> = None;
    let mut cell_type: Option<String> = None;
    let mut source: Option<String> = None;
    let mut execution_count = "null".to_string();
    let mut outputs: Vec<String> = Vec::new();

    // next_object() returns the first key (or None if empty object)
    // Convert to owned String to release the borrow
    let first_key = jiter.next_object()?.map(|s| s.to_string());

    if let Some(key) = first_key {
        process_cell_field(
            &key,
            jiter,
            &mut id,
            &mut cell_type,
            &mut source,
            &mut execution_count,
            &mut outputs,
        )?;

        // Process remaining fields
        loop {
            let next_key = jiter.next_key()?.map(|s| s.to_string());
            match next_key {
                Some(key) => {
                    process_cell_field(
                        &key,
                        jiter,
                        &mut id,
                        &mut cell_type,
                        &mut source,
                        &mut execution_count,
                        &mut outputs,
                    )?;
                }
                None => break,
            }
        }
    }

    // Generate stable fallback ID for older notebooks without cell IDs
    let id = id.unwrap_or_else(|| format!("__external_cell_{}", index));
    let cell_type = cell_type.unwrap_or_else(|| "code".to_string());
    let source = source.unwrap_or_default();

    Ok(CellSnapshot {
        id,
        cell_type,
        source,
        execution_count,
        outputs,
    })
}

/// Process a single cell field.
#[allow(clippy::too_many_arguments)]
fn process_cell_field(
    key: &str,
    jiter: &mut Jiter,
    id: &mut Option<String>,
    cell_type: &mut Option<String>,
    source: &mut Option<String>,
    execution_count: &mut String,
    outputs: &mut Vec<String>,
) -> Result<(), StreamError> {
    match key {
        "id" => {
            *id = match jiter.peek()? {
                Peek::String => Some(jiter.next_str()?.to_string()),
                _ => {
                    jiter.next_skip()?;
                    None
                }
            };
        }
        "cell_type" => {
            *cell_type = match jiter.peek()? {
                Peek::String => Some(jiter.next_str()?.to_string()),
                _ => {
                    jiter.next_skip()?;
                    None
                }
            };
        }
        "source" => {
            *source = Some(parse_source(jiter)?);
        }
        "execution_count" => {
            *execution_count = match jiter.peek()? {
                Peek::Null => {
                    jiter.next_null()?;
                    "null".to_string()
                }
                Peek::True | Peek::False => {
                    jiter.next_skip()?;
                    "null".to_string()
                }
                _ => {
                    // Could be number
                    let val = jiter.next_value()?;
                    match val {
                        JsonValue::Int(n) => n.to_string(),
                        JsonValue::BigInt(n) => n.to_string(),
                        JsonValue::Float(n) => (n as i64).to_string(),
                        _ => "null".to_string(),
                    }
                }
            };
        }
        "outputs" => {
            *outputs = parse_outputs(jiter)?;
        }
        _ => {
            // Skip metadata, attachments, etc.
            jiter.next_skip()?;
        }
    }
    Ok(())
}

/// Parse cell source which can be a string or array of strings.
fn parse_source(jiter: &mut Jiter) -> Result<String, StreamError> {
    match jiter.peek()? {
        Peek::String => Ok(jiter.next_str()?.to_string()),
        Peek::Array => {
            let mut result = String::new();
            // next_array() returns Option<Peek> for first element
            if let Some(first_peek) = jiter.next_array()? {
                if first_peek == Peek::String {
                    result.push_str(jiter.next_str()?);
                } else {
                    jiter.next_skip()?;
                }

                // Process remaining elements
                while let Some(peek) = jiter.array_step()? {
                    if peek == Peek::String {
                        result.push_str(jiter.next_str()?);
                    } else {
                        jiter.next_skip()?;
                    }
                }
            }
            Ok(result)
        }
        _ => {
            jiter.next_skip()?;
            Ok(String::new())
        }
    }
}

/// Parse outputs array, serializing each output as a JSON string.
fn parse_outputs(jiter: &mut Jiter) -> Result<Vec<String>, StreamError> {
    match jiter.peek()? {
        Peek::Array => {
            let mut outputs = Vec::new();
            // next_array() returns Option<Peek> for first element
            if let Some(first_peek) = jiter.next_array()? {
                if first_peek == Peek::Object {
                    let output_value = jiter.next_value()?;
                    outputs.push(json_value_to_string(&output_value));
                } else {
                    jiter.next_skip()?;
                }

                // Process remaining elements
                while let Some(peek) = jiter.array_step()? {
                    if peek == Peek::Object {
                        let output_value = jiter.next_value()?;
                        outputs.push(json_value_to_string(&output_value));
                    } else {
                        jiter.next_skip()?;
                    }
                }
            }
            Ok(outputs)
        }
        _ => {
            jiter.next_skip()?;
            Ok(Vec::new())
        }
    }
}

/// Convert jiter JsonValue to JSON string.
fn json_value_to_string(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "null".to_string(),
        JsonValue::Bool(b) => b.to_string(),
        JsonValue::Int(n) => n.to_string(),
        JsonValue::BigInt(n) => n.to_string(),
        JsonValue::Float(n) => n.to_string(),
        JsonValue::Str(s) => format!("\"{}\"", escape_json_string(s)),
        JsonValue::Array(arr) => {
            let items: Vec<String> = arr.iter().map(json_value_to_string).collect();
            format!("[{}]", items.join(","))
        }
        JsonValue::Object(obj) => {
            let items: Vec<String> = obj
                .iter()
                .map(|(k, v)| format!("\"{}\":{}", escape_json_string(k), json_value_to_string(v)))
                .collect();
            format!("{{{}}}", items.join(","))
        }
    }
}

/// Escape special characters in a JSON string.
fn escape_json_string(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            c if c.is_control() => {
                result.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => result.push(c),
        }
    }
    result
}

/// Parse notebook metadata from a jiter JsonValue.
fn parse_metadata_from_value(value: &JsonValue) -> Result<NotebookMetadataSnapshot, StreamError> {
    // Convert JsonValue to serde_json::Value for compatibility with existing code
    let json_str = json_value_to_string(value);
    let serde_value: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|e| StreamError::InvalidStructure(format!("Failed to parse metadata: {}", e)))?;

    Ok(NotebookMetadataSnapshot::from_metadata_value(&serde_value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_notebook() {
        let notebook_json = r##"{
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {
                "kernelspec": {
                    "name": "python3",
                    "display_name": "Python 3"
                }
            },
            "cells": [
                {
                    "id": "cell-1",
                    "cell_type": "code",
                    "source": "print('hello')",
                    "execution_count": 1,
                    "outputs": []
                },
                {
                    "id": "cell-2",
                    "cell_type": "markdown",
                    "source": ["# Header\n", "Some text"],
                    "outputs": []
                }
            ]
        }"##;

        let mut cells = Vec::new();
        let result = stream_notebook_cells(notebook_json.as_bytes(), |cell, _idx| {
            cells.push(cell);
            Ok(())
        })
        .unwrap();

        assert_eq!(result.cell_count, 2);
        assert_eq!(cells.len(), 2);

        assert_eq!(cells[0].id, "cell-1");
        assert_eq!(cells[0].cell_type, "code");
        assert_eq!(cells[0].source, "print('hello')");
        assert_eq!(cells[0].execution_count, "1");

        assert_eq!(cells[1].id, "cell-2");
        assert_eq!(cells[1].cell_type, "markdown");
        assert_eq!(cells[1].source, "# Header\nSome text");
    }

    #[test]
    fn test_parse_cell_without_id() {
        let notebook_json = r#"{
            "cells": [
                {
                    "cell_type": "code",
                    "source": "x = 1"
                }
            ]
        }"#;

        let mut cells = Vec::new();
        stream_notebook_cells(notebook_json.as_bytes(), |cell, _idx| {
            cells.push(cell);
            Ok(())
        })
        .unwrap();

        assert_eq!(cells[0].id, "__external_cell_0");
    }

    #[test]
    fn test_parse_outputs() {
        let notebook_json = r#"{
            "cells": [
                {
                    "id": "cell-1",
                    "cell_type": "code",
                    "source": "1+1",
                    "execution_count": 1,
                    "outputs": [
                        {
                            "output_type": "execute_result",
                            "data": {"text/plain": "2"},
                            "execution_count": 1
                        }
                    ]
                }
            ]
        }"#;

        let mut cells = Vec::new();
        stream_notebook_cells(notebook_json.as_bytes(), |cell, _idx| {
            cells.push(cell);
            Ok(())
        })
        .unwrap();

        assert_eq!(cells[0].outputs.len(), 1);
        assert!(cells[0].outputs[0].contains("execute_result"));
    }
}
