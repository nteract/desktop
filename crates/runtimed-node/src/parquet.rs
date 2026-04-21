//! Parquet summarization and row reading for the TUI table viewer.
//!
//! Takes raw parquet bytes (from `application/vnd.apache.parquet` cell output)
//! and returns structured summaries + row data for TUI rendering.

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

/// Summarize a parquet file from its raw bytes (base64-encoded).
#[napi]
pub fn summarize_parquet(base64_data: String) -> napi::Result<ParquetSummaryResult> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let bytes = STANDARD
        .decode(&base64_data)
        .map_err(|e| napi::Error::from_reason(format!("Invalid base64: {e}")))?;

    let summary = parquet_summary::summarize_parquet(&bytes)
        .map_err(|e| napi::Error::from_reason(format!("Parquet error: {e}")))?;

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

/// Read a page of rows from parquet bytes (base64-encoded).
/// Returns string representations of each cell for display.
#[napi]
pub fn read_parquet_rows(
    base64_data: String,
    offset: i64,
    limit: i64,
) -> napi::Result<ParquetRowPage> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let bytes = STANDARD
        .decode(&base64_data)
        .map_err(|e| napi::Error::from_reason(format!("Invalid base64: {e}")))?;

    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes.clone()))
        .map_err(|e| napi::Error::from_reason(format!("Parquet error: {e}")))?;

    let schema = builder.schema().clone();
    let columns: Vec<String> = schema.fields().iter().map(|f| f.name().clone()).collect();
    let reader = builder
        .build()
        .map_err(|e| napi::Error::from_reason(format!("Parquet reader error: {e}")))?;

    let offset = offset.max(0) as usize;
    let limit = limit.max(0) as usize;
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut total_rows: usize = 0;
    let mut row_idx: usize = 0;

    for batch in reader {
        let batch =
            batch.map_err(|e| napi::Error::from_reason(format!("Batch read error: {e}")))?;
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
