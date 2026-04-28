//! Parquet summarization and row reading for the TUI table viewer.
//!
//! Takes raw parquet bytes or blob file paths (from
//! `application/vnd.apache.parquet` cell output) and returns structured
//! summaries + row data for TUI rendering.

use napi_derive::napi;

use arrow::array::Array;
use nteract_predicate::parquet_summary;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

/// Summary of a parquet dataset — column names, types, stats.
#[napi(object)]
pub struct ParquetSummaryResult {
    pub num_rows: i64,
    pub num_bytes: i64,
    pub columns: Vec<ParquetColumnInfo>,
}

#[napi(object)]
pub struct ParquetColumnInfo {
    pub name: String,
    pub data_type: String,
    pub null_count: i64,
    /// JSON-encoded column stats (numeric: {min, max}, string: {distinct_count, top}, etc.)
    pub stats_json: String,
}

/// A page of rows from a parquet file, as string values for display.
#[napi(object)]
pub struct ParquetRowPage {
    pub columns: Vec<String>,
    /// Row data — outer vec is rows, inner vec is column values as display strings.
    pub rows: Vec<Vec<String>>,
    pub total_rows: i64,
    pub offset: i64,
}

/// Summarize a parquet file from a local blob/file path.
#[napi]
pub fn summarize_parquet_file(file_path: String) -> napi::Result<ParquetSummaryResult> {
    let bytes = std::fs::read(&file_path)
        .map_err(|e| napi::Error::from_reason(format!("Failed to read {file_path}: {e}")))?;
    summarize_parquet_from_bytes(&bytes).map_err(napi::Error::from_reason)
}

fn summarize_parquet_from_bytes(bytes: &[u8]) -> Result<ParquetSummaryResult, String> {
    let summary =
        parquet_summary::summarize_parquet(bytes).map_err(|e| format!("Parquet error: {e}"))?;

    let columns = summary
        .columns
        .iter()
        .map(|c| ParquetColumnInfo {
            name: c.name.clone(),
            data_type: c.data_type.clone(),
            null_count: c.null_count as i64,
            stats_json: serde_json::to_string(&c.stats).unwrap_or_default(),
        })
        .collect();

    Ok(ParquetSummaryResult {
        num_rows: summary.num_rows as i64,
        num_bytes: summary.num_bytes as i64,
        columns,
    })
}

/// Read a page of rows from a local blob/file path.
/// Returns string representations of each cell for display.
#[napi]
pub fn read_parquet_file(
    file_path: String,
    offset: i64,
    limit: i64,
) -> napi::Result<ParquetRowPage> {
    let bytes = std::fs::read(&file_path)
        .map_err(|e| napi::Error::from_reason(format!("Failed to read {file_path}: {e}")))?;
    read_parquet_rows_from_bytes(bytes, offset, limit).map_err(napi::Error::from_reason)
}

fn read_parquet_rows_from_bytes(
    bytes: Vec<u8>,
    offset: i64,
    limit: i64,
) -> Result<ParquetRowPage, String> {
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes))
        .map_err(|e| format!("Parquet error: {e}"))?;

    let schema = builder.schema().clone();
    let columns: Vec<String> = schema.fields().iter().map(|f| f.name().clone()).collect();
    let reader = builder
        .build()
        .map_err(|e| format!("Parquet reader error: {e}"))?;

    let offset = offset.max(0) as usize;
    let limit = limit.max(0) as usize;
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut total_rows: usize = 0;
    let mut row_idx: usize = 0;

    for batch in reader {
        let batch = batch.map_err(|e| format!("Batch read error: {e}"))?;
        let batch_rows = batch.num_rows();
        total_rows += batch_rows;

        // Skip batches before offset
        if row_idx + batch_rows <= offset {
            row_idx += batch_rows;
            continue;
        }

        // Read rows from this batch
        let start = offset.saturating_sub(row_idx);
        let end = batch_rows.min(start + limit - rows.len());

        for r in start..end {
            if rows.len() >= limit {
                break;
            }
            let mut row: Vec<String> = Vec::with_capacity(batch.num_columns());
            for col in batch.columns() {
                row.push(array_value_to_string(col.as_ref(), r));
            }
            rows.push(row);
        }

        row_idx += batch_rows;
        if rows.len() >= limit {
            // Still need to count remaining rows
            continue;
        }
    }

    Ok(ParquetRowPage {
        columns,
        rows,
        total_rows: total_rows as i64,
        offset: offset as i64,
    })
}

/// Convert an Arrow array value at a given index to a display string.
fn array_value_to_string(array: &dyn Array, idx: usize) -> String {
    if array.is_null(idx) {
        return "null".to_string();
    }

    use arrow::array::*;
    use arrow::datatypes::DataType;

    match array.data_type() {
        DataType::Utf8 => array
            .as_any()
            .downcast_ref::<StringArray>()
            .map(|a| a.value(idx).to_string())
            .unwrap_or_default(),
        DataType::LargeUtf8 => array
            .as_any()
            .downcast_ref::<LargeStringArray>()
            .map(|a| a.value(idx).to_string())
            .unwrap_or_default(),
        DataType::Int64 => array
            .as_any()
            .downcast_ref::<Int64Array>()
            .map(|a| a.value(idx).to_string())
            .unwrap_or_default(),
        DataType::Int32 => array
            .as_any()
            .downcast_ref::<Int32Array>()
            .map(|a| a.value(idx).to_string())
            .unwrap_or_default(),
        DataType::Float64 => array
            .as_any()
            .downcast_ref::<Float64Array>()
            .map(|a| format!("{:.4}", a.value(idx)))
            .unwrap_or_default(),
        DataType::Float32 => array
            .as_any()
            .downcast_ref::<Float32Array>()
            .map(|a| format!("{:.4}", a.value(idx)))
            .unwrap_or_default(),
        DataType::Boolean => array
            .as_any()
            .downcast_ref::<BooleanArray>()
            .map(|a| a.value(idx).to_string())
            .unwrap_or_default(),
        DataType::UInt64 => array
            .as_any()
            .downcast_ref::<UInt64Array>()
            .map(|a| a.value(idx).to_string())
            .unwrap_or_default(),
        DataType::UInt32 => array
            .as_any()
            .downcast_ref::<UInt32Array>()
            .map(|a| a.value(idx).to_string())
            .unwrap_or_default(),
        DataType::Dictionary(_, _) => {
            // Use the shared helper for dict-encoded strings
            nteract_predicate::arrow_utils::string_at(array, idx).unwrap_or_else(|| "?".to_string())
        }
        _ => {
            // Fallback: use Arrow's built-in display
            arrow::util::display::array_value_to_string(array, idx)
                .unwrap_or_else(|_| "?".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use arrow::array::{BooleanArray, Float64Array, Int32Array, StringArray};
    use arrow::datatypes::{DataType, Field, Schema};
    use arrow::record_batch::RecordBatch;
    use parquet::arrow::ArrowWriter;
    use std::io::Cursor;
    use std::sync::Arc;

    fn sample_parquet_bytes() -> Vec<u8> {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int32, false),
            Field::new("label", DataType::Utf8, true),
            Field::new("score", DataType::Float64, false),
            Field::new("flag", DataType::Boolean, false),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int32Array::from(vec![1, 2, 3])),
                Arc::new(StringArray::from(vec![Some("alpha"), None, Some("gamma")])),
                Arc::new(Float64Array::from(vec![1.25, 2.5, 3.75])),
                Arc::new(BooleanArray::from(vec![true, false, true])),
            ],
        )
        .expect("record batch");

        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer =
                ArrowWriter::try_new(&mut cursor, schema, None).expect("parquet writer");
            writer.write(&batch).expect("write batch");
            writer.close().expect("close writer");
        }
        cursor.into_inner()
    }

    #[test]
    fn summarize_parquet_reports_rows_bytes_and_columns() {
        let bytes = sample_parquet_bytes();
        let summary = summarize_parquet_from_bytes(&bytes).expect("summary");

        assert_eq!(summary.num_rows, 3);
        assert!(summary.num_bytes > 0);
        assert_eq!(
            summary
                .columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["id", "label", "score", "flag"]
        );
        assert_eq!(summary.columns[1].data_type, "string");
        assert_eq!(summary.columns[1].null_count, 1);
        assert!(serde_json::from_str::<serde_json::Value>(&summary.columns[0].stats_json).is_ok());
    }

    #[test]
    fn read_parquet_rows_paginates_and_formats_values_for_js() {
        let page = read_parquet_rows_from_bytes(sample_parquet_bytes(), 1, 2).expect("rows");

        assert_eq!(page.columns, vec!["id", "label", "score", "flag"]);
        assert_eq!(page.total_rows, 3);
        assert_eq!(page.offset, 1);
        assert_eq!(
            page.rows,
            vec![
                vec![
                    "2".to_string(),
                    "null".to_string(),
                    "2.5000".to_string(),
                    "false".to_string()
                ],
                vec![
                    "3".to_string(),
                    "gamma".to_string(),
                    "3.7500".to_string(),
                    "true".to_string()
                ],
            ]
        );
    }

    #[test]
    fn read_parquet_rows_clamps_negative_offset_and_limit() {
        let page = read_parquet_rows_from_bytes(sample_parquet_bytes(), -10, -1).expect("rows");

        assert_eq!(page.total_rows, 3);
        assert_eq!(page.offset, 0);
        assert!(page.rows.is_empty());
    }

    #[test]
    fn invalid_parquet_bytes_report_a_parquet_error() {
        match summarize_parquet_from_bytes(b"not parquet") {
            Ok(_) => panic!("invalid parquet bytes should fail"),
            Err(err) => assert!(err.contains("Parquet error")),
        }
    }

    #[test]
    fn parquet_file_helpers_read_local_blob_paths() {
        let tmp = std::env::temp_dir().join(format!(
            "runtimed-node-parquet-{}.parquet",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&tmp, sample_parquet_bytes()).expect("write parquet");
        let path = tmp.to_string_lossy().to_string();

        let summary = summarize_parquet_file(path.clone()).expect("summary");
        let page = read_parquet_file(path, 0, 1).expect("rows");
        let _ = std::fs::remove_file(tmp);

        assert_eq!(summary.num_rows, 3);
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0][0], "1");
    }
}
