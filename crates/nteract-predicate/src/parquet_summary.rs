//! Parquet summarization for LLM text representations.
//!
//! Reads Parquet bytes and produces a structured summary: row count,
//! column types, per-column stats (null count, min/max for numerics,
//! top values for strings, true/false counts for booleans).
//!
//! Designed to be rendered as `text/llm+plain` so agents can understand
//! a dataset without rendering it.

use arrow::array::{
    Array, AsArray, BooleanArray, Date32Array, Date64Array, Float32Array, Float64Array, Int16Array,
    Int32Array, Int64Array, Int8Array, LargeStringArray, StringArray, TimestampMicrosecondArray,
    TimestampMillisecondArray, TimestampNanosecondArray, TimestampSecondArray, UInt16Array,
    UInt32Array, UInt64Array, UInt8Array,
};
use arrow::datatypes::{DataType, TimeUnit};
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::Serialize;
use std::collections::HashMap;

use crate::utils::dict_key_at;

/// Maximum number of distinct values to enumerate for categorical columns.
const TOP_N_CATEGORIES: usize = 5;

/// Top-level summary of a Parquet dataset.
#[derive(Serialize, Debug, Clone)]
pub struct ParquetSummary {
    /// Total number of rows across all batches.
    pub num_rows: u64,
    /// Uncompressed size estimate in bytes (file bytes).
    pub num_bytes: u64,
    /// One entry per column, in schema order.
    pub columns: Vec<ColumnSummary>,
}

/// Summary of a single column.
#[derive(Serialize, Debug, Clone)]
pub struct ColumnSummary {
    pub name: String,
    /// Arrow DataType rendered as a human-readable string.
    pub data_type: String,
    /// Number of nulls across all batches.
    pub null_count: u64,
    /// Column-type-specific summary.
    pub stats: ColumnStats,
}

#[derive(Serialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ColumnStats {
    /// Numeric: min/max as f64 (lossy for i64, but fine for summaries).
    Numeric { min: f64, max: f64 },
    /// Boolean: counts of true/false (nulls already in ColumnSummary).
    Boolean { true_count: u64, false_count: u64 },
    /// String/categorical: top N values plus total distinct count.
    String {
        distinct_count: u64,
        top: Vec<(String, u64)>,
    },
    /// Temporal: min/max formatted as ISO strings.
    Temporal { min: String, max: String },
    /// Types we don't summarize (structs, lists, maps, etc).
    Other,
}

/// Summarize a Parquet file from its raw bytes.
pub fn summarize_parquet(bytes: &[u8]) -> Result<ParquetSummary, Box<dyn std::error::Error>> {
    let bytes_vec = bytes.to_vec();
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes_vec))?;
    let schema = builder.schema().clone();
    let reader = builder.build()?;

    let num_cols = schema.fields().len();
    let mut null_counts: Vec<u64> = vec![0; num_cols];
    let mut numeric_accum: Vec<Option<(f64, f64)>> = vec![None; num_cols];
    let mut bool_accum: Vec<(u64, u64)> = vec![(0, 0); num_cols];
    let mut string_accum: Vec<HashMap<String, u64>> =
        (0..num_cols).map(|_| HashMap::new()).collect();
    let mut temporal_accum: Vec<Option<(i64, i64, TimeUnit)>> = vec![None; num_cols];
    let mut total_rows: u64 = 0;

    for batch in reader {
        let batch = batch?;
        total_rows += batch.num_rows() as u64;

        for (col_idx, col) in batch.columns().iter().enumerate() {
            null_counts[col_idx] += col.null_count() as u64;
            accumulate_column_stats(
                col.as_ref(),
                col_idx,
                &mut numeric_accum,
                &mut bool_accum,
                &mut string_accum,
                &mut temporal_accum,
            );
        }
        // Keep heavy string accums bounded — if we've already seen many distinct values,
        // we don't need to keep collecting (top-N is what matters).
        if total_rows > 100_000 {
            for map in string_accum.iter_mut() {
                if map.len() > 1_000 {
                    // Keep the top 50 by count; drop the rest to bound memory.
                    let mut pairs: Vec<_> = map.drain().collect();
                    pairs.sort_unstable_by(|a, b| b.1.cmp(&a.1));
                    pairs.truncate(50);
                    map.extend(pairs);
                }
            }
        }
    }

    let columns: Vec<ColumnSummary> = schema
        .fields()
        .iter()
        .enumerate()
        .map(|(i, field)| {
            let dt = field.data_type();
            let stats = finalize_column_stats(
                dt,
                &numeric_accum[i],
                &bool_accum[i],
                &string_accum[i],
                &temporal_accum[i],
            );
            ColumnSummary {
                name: field.name().clone(),
                data_type: format_data_type(dt),
                null_count: null_counts[i],
                stats,
            }
        })
        .collect();

    Ok(ParquetSummary {
        num_rows: total_rows,
        num_bytes: bytes.len() as u64,
        columns,
    })
}

/// Summarize an already-loaded RecordBatch iterator.
/// Useful for tests and for cases where callers already have Arrow data.
pub fn summarize_record_batches(
    batches: &[RecordBatch],
) -> Result<ParquetSummary, Box<dyn std::error::Error>> {
    if batches.is_empty() {
        return Ok(ParquetSummary {
            num_rows: 0,
            num_bytes: 0,
            columns: Vec::new(),
        });
    }
    let schema = batches[0].schema();
    let num_cols = schema.fields().len();
    let mut null_counts: Vec<u64> = vec![0; num_cols];
    let mut numeric_accum: Vec<Option<(f64, f64)>> = vec![None; num_cols];
    let mut bool_accum: Vec<(u64, u64)> = vec![(0, 0); num_cols];
    let mut string_accum: Vec<HashMap<String, u64>> =
        (0..num_cols).map(|_| HashMap::new()).collect();
    let mut temporal_accum: Vec<Option<(i64, i64, TimeUnit)>> = vec![None; num_cols];
    let mut total_rows: u64 = 0;

    for batch in batches {
        total_rows += batch.num_rows() as u64;
        for (col_idx, col) in batch.columns().iter().enumerate() {
            null_counts[col_idx] += col.null_count() as u64;
            accumulate_column_stats(
                col.as_ref(),
                col_idx,
                &mut numeric_accum,
                &mut bool_accum,
                &mut string_accum,
                &mut temporal_accum,
            );
        }
    }

    let columns: Vec<ColumnSummary> = schema
        .fields()
        .iter()
        .enumerate()
        .map(|(i, field)| {
            let dt = field.data_type();
            let stats = finalize_column_stats(
                dt,
                &numeric_accum[i],
                &bool_accum[i],
                &string_accum[i],
                &temporal_accum[i],
            );
            ColumnSummary {
                name: field.name().clone(),
                data_type: format_data_type(dt),
                null_count: null_counts[i],
                stats,
            }
        })
        .collect();

    Ok(ParquetSummary {
        num_rows: total_rows,
        num_bytes: 0, // unknown for already-decoded batches
        columns,
    })
}

fn accumulate_column_stats(
    col: &dyn Array,
    col_idx: usize,
    numeric_accum: &mut [Option<(f64, f64)>],
    bool_accum: &mut [(u64, u64)],
    string_accum: &mut [HashMap<String, u64>],
    temporal_accum: &mut [Option<(i64, i64, TimeUnit)>],
) {
    match col.data_type() {
        DataType::Float64 => {
            if let Some(arr) = col.as_any().downcast_ref::<Float64Array>() {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        update_numeric(&mut numeric_accum[col_idx], arr.value(i));
                    }
                }
            }
        }
        DataType::Float32 => {
            if let Some(arr) = col.as_any().downcast_ref::<Float32Array>() {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        update_numeric(&mut numeric_accum[col_idx], arr.value(i) as f64);
                    }
                }
            }
        }
        DataType::Int64 => {
            if let Some(arr) = col.as_any().downcast_ref::<Int64Array>() {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        update_numeric(&mut numeric_accum[col_idx], arr.value(i) as f64);
                    }
                }
            }
        }
        DataType::Int32 => {
            if let Some(arr) = col.as_any().downcast_ref::<Int32Array>() {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        update_numeric(&mut numeric_accum[col_idx], arr.value(i) as f64);
                    }
                }
            }
        }
        DataType::Int16 => {
            if let Some(arr) = col.as_any().downcast_ref::<Int16Array>() {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        update_numeric(&mut numeric_accum[col_idx], arr.value(i) as f64);
                    }
                }
            }
        }
        DataType::Int8 => {
            if let Some(arr) = col.as_any().downcast_ref::<Int8Array>() {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        update_numeric(&mut numeric_accum[col_idx], arr.value(i) as f64);
                    }
                }
            }
        }
        DataType::UInt64 => {
            if let Some(arr) = col.as_any().downcast_ref::<UInt64Array>() {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        update_numeric(&mut numeric_accum[col_idx], arr.value(i) as f64);
                    }
                }
            }
        }
        DataType::UInt32 => {
            if let Some(arr) = col.as_any().downcast_ref::<UInt32Array>() {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        update_numeric(&mut numeric_accum[col_idx], arr.value(i) as f64);
                    }
                }
            }
        }
        DataType::UInt16 => {
            if let Some(arr) = col.as_any().downcast_ref::<UInt16Array>() {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        update_numeric(&mut numeric_accum[col_idx], arr.value(i) as f64);
                    }
                }
            }
        }
        DataType::UInt8 => {
            if let Some(arr) = col.as_any().downcast_ref::<UInt8Array>() {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        update_numeric(&mut numeric_accum[col_idx], arr.value(i) as f64);
                    }
                }
            }
        }
        DataType::Boolean => {
            if let Some(arr) = col.as_any().downcast_ref::<BooleanArray>() {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        if arr.value(i) {
                            bool_accum[col_idx].0 += 1;
                        } else {
                            bool_accum[col_idx].1 += 1;
                        }
                    }
                }
            }
        }
        DataType::Utf8 => {
            if let Some(arr) = col.as_any().downcast_ref::<StringArray>() {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        *string_accum[col_idx]
                            .entry(arr.value(i).to_string())
                            .or_insert(0) += 1;
                    }
                }
            }
        }
        DataType::LargeUtf8 => {
            if let Some(arr) = col.as_any().downcast_ref::<LargeStringArray>() {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        *string_accum[col_idx]
                            .entry(arr.value(i).to_string())
                            .or_insert(0) += 1;
                    }
                }
            }
        }
        DataType::Dictionary(_, _) => {
            let dict_arr = col.as_any_dictionary();
            let keys = dict_arr.keys();
            let values = dict_arr.values();
            if let Some(str_values) = values.as_any().downcast_ref::<StringArray>() {
                for i in 0..keys.len() {
                    if let Some(key) = dict_key_at(keys, i) {
                        *string_accum[col_idx]
                            .entry(str_values.value(key).to_string())
                            .or_insert(0) += 1;
                    }
                }
            } else if let Some(str_values) = values.as_any().downcast_ref::<LargeStringArray>() {
                for i in 0..keys.len() {
                    if let Some(key) = dict_key_at(keys, i) {
                        *string_accum[col_idx]
                            .entry(str_values.value(key).to_string())
                            .or_insert(0) += 1;
                    }
                }
            }
        }
        DataType::Timestamp(unit, _) => {
            update_temporal(&mut temporal_accum[col_idx], col, *unit);
        }
        DataType::Date32 => {
            if let Some(arr) = col.as_any().downcast_ref::<Date32Array>() {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        update_temporal_i64(
                            &mut temporal_accum[col_idx],
                            arr.value(i) as i64,
                            TimeUnit::Second,
                        );
                    }
                }
            }
        }
        DataType::Date64 => {
            if let Some(arr) = col.as_any().downcast_ref::<Date64Array>() {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        update_temporal_i64(
                            &mut temporal_accum[col_idx],
                            arr.value(i),
                            TimeUnit::Millisecond,
                        );
                    }
                }
            }
        }
        _ => {} // Other — no stats
    }
}

fn update_numeric(accum: &mut Option<(f64, f64)>, v: f64) {
    if !v.is_finite() {
        return;
    }
    match accum {
        None => *accum = Some((v, v)),
        Some((min, max)) => {
            if v < *min {
                *min = v;
            }
            if v > *max {
                *max = v;
            }
        }
    }
}

fn update_temporal(accum: &mut Option<(i64, i64, TimeUnit)>, col: &dyn Array, unit: TimeUnit) {
    match unit {
        TimeUnit::Nanosecond => {
            if let Some(arr) = col.as_any().downcast_ref::<TimestampNanosecondArray>() {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        update_temporal_i64(accum, arr.value(i), unit);
                    }
                }
            }
        }
        TimeUnit::Microsecond => {
            if let Some(arr) = col.as_any().downcast_ref::<TimestampMicrosecondArray>() {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        update_temporal_i64(accum, arr.value(i), unit);
                    }
                }
            }
        }
        TimeUnit::Millisecond => {
            if let Some(arr) = col.as_any().downcast_ref::<TimestampMillisecondArray>() {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        update_temporal_i64(accum, arr.value(i), unit);
                    }
                }
            }
        }
        TimeUnit::Second => {
            if let Some(arr) = col.as_any().downcast_ref::<TimestampSecondArray>() {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        update_temporal_i64(accum, arr.value(i), unit);
                    }
                }
            }
        }
    }
}

fn update_temporal_i64(accum: &mut Option<(i64, i64, TimeUnit)>, v: i64, unit: TimeUnit) {
    match accum {
        None => *accum = Some((v, v, unit)),
        Some((min, max, _)) => {
            if v < *min {
                *min = v;
            }
            if v > *max {
                *max = v;
            }
        }
    }
}

fn finalize_column_stats(
    dt: &DataType,
    numeric: &Option<(f64, f64)>,
    bool_counts: &(u64, u64),
    string_counts: &HashMap<String, u64>,
    temporal: &Option<(i64, i64, TimeUnit)>,
) -> ColumnStats {
    match dt {
        DataType::Float16
        | DataType::Float32
        | DataType::Float64
        | DataType::Int8
        | DataType::Int16
        | DataType::Int32
        | DataType::Int64
        | DataType::UInt8
        | DataType::UInt16
        | DataType::UInt32
        | DataType::UInt64 => match numeric {
            Some((min, max)) => ColumnStats::Numeric {
                min: *min,
                max: *max,
            },
            None => ColumnStats::Numeric {
                min: f64::NAN,
                max: f64::NAN,
            },
        },
        DataType::Boolean => ColumnStats::Boolean {
            true_count: bool_counts.0,
            false_count: bool_counts.1,
        },
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Dictionary(_, _) => {
            let mut pairs: Vec<(String, u64)> =
                string_counts.iter().map(|(k, v)| (k.clone(), *v)).collect();
            pairs.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let top: Vec<(String, u64)> = pairs.iter().take(TOP_N_CATEGORIES).cloned().collect();
            ColumnStats::String {
                distinct_count: string_counts.len() as u64,
                top,
            }
        }
        DataType::Timestamp(_, _) | DataType::Date32 | DataType::Date64 => match temporal {
            Some((min, max, unit)) => ColumnStats::Temporal {
                min: format_temporal(*min, *unit, dt),
                max: format_temporal(*max, *unit, dt),
            },
            None => ColumnStats::Temporal {
                min: String::new(),
                max: String::new(),
            },
        },
        _ => ColumnStats::Other,
    }
}

fn format_temporal(value: i64, unit: TimeUnit, dt: &DataType) -> String {
    use chrono::{DateTime, Utc};
    let nanos = match unit {
        TimeUnit::Second => value.saturating_mul(1_000_000_000),
        TimeUnit::Millisecond => value.saturating_mul(1_000_000),
        TimeUnit::Microsecond => value.saturating_mul(1_000),
        TimeUnit::Nanosecond => value,
    };
    let secs = nanos.div_euclid(1_000_000_000);
    let subnanos = nanos.rem_euclid(1_000_000_000) as u32;
    let dt_utc = DateTime::<Utc>::from_timestamp(secs, subnanos);
    match dt_utc {
        Some(ts) => match dt {
            DataType::Date32 | DataType::Date64 => ts.format("%Y-%m-%d").to_string(),
            _ => ts.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        },
        None => format!("{value}"),
    }
}

fn format_data_type(dt: &DataType) -> String {
    match dt {
        DataType::Utf8 | DataType::LargeUtf8 => "string".to_string(),
        DataType::Boolean => "bool".to_string(),
        DataType::Int8 => "int8".to_string(),
        DataType::Int16 => "int16".to_string(),
        DataType::Int32 => "int32".to_string(),
        DataType::Int64 => "int64".to_string(),
        DataType::UInt8 => "uint8".to_string(),
        DataType::UInt16 => "uint16".to_string(),
        DataType::UInt32 => "uint32".to_string(),
        DataType::UInt64 => "uint64".to_string(),
        DataType::Float32 => "float32".to_string(),
        DataType::Float64 => "float64".to_string(),
        DataType::Timestamp(unit, _) => format!("timestamp[{unit:?}]").to_lowercase(),
        DataType::Date32 => "date32".to_string(),
        DataType::Date64 => "date64".to_string(),
        DataType::Dictionary(_, value_type) => format!("dict[{}]", format_data_type(value_type)),
        _ => format!("{dt:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{BooleanArray, Float64Array, Int64Array, StringArray};
    use arrow::datatypes::{Field, Schema};
    use arrow::record_batch::RecordBatch;
    use parquet::arrow::ArrowWriter;
    use parquet::file::properties::WriterProperties;
    use std::sync::Arc;

    fn make_test_batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("name", DataType::Utf8, true),
            Field::new("score", DataType::Float64, false),
            Field::new("active", DataType::Boolean, false),
        ]));
        let ids = Int64Array::from(vec![1, 2, 3, 4, 5]);
        let names = StringArray::from(vec![
            Some("alice"),
            Some("bob"),
            None,
            Some("alice"),
            Some("carol"),
        ]);
        let scores = Float64Array::from(vec![0.5, 0.7, 0.1, 0.9, 0.3]);
        let active = BooleanArray::from(vec![true, false, true, true, false]);
        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(ids),
                Arc::new(names),
                Arc::new(scores),
                Arc::new(active),
            ],
        )
        .unwrap()
    }

    fn batch_to_parquet_bytes(batch: &RecordBatch) -> Vec<u8> {
        let mut buf = Vec::new();
        let props = WriterProperties::builder().build();
        let mut writer = ArrowWriter::try_new(&mut buf, batch.schema(), Some(props)).unwrap();
        writer.write(batch).unwrap();
        writer.close().unwrap();
        buf
    }

    #[test]
    fn summarize_parquet_basic() {
        let batch = make_test_batch();
        let bytes = batch_to_parquet_bytes(&batch);
        let summary = summarize_parquet(&bytes).unwrap();

        assert_eq!(summary.num_rows, 5);
        assert_eq!(summary.columns.len(), 4);

        assert_eq!(summary.columns[0].name, "id");
        assert_eq!(summary.columns[0].data_type, "int64");
        assert_eq!(summary.columns[0].null_count, 0);
        match &summary.columns[0].stats {
            ColumnStats::Numeric { min, max } => {
                assert_eq!(*min, 1.0);
                assert_eq!(*max, 5.0);
            }
            _ => panic!("expected numeric stats"),
        }

        assert_eq!(summary.columns[1].name, "name");
        assert_eq!(summary.columns[1].null_count, 1);
        match &summary.columns[1].stats {
            ColumnStats::String {
                distinct_count,
                top,
            } => {
                assert_eq!(*distinct_count, 3);
                assert_eq!(top[0], ("alice".to_string(), 2));
            }
            _ => panic!("expected string stats"),
        }

        match &summary.columns[3].stats {
            ColumnStats::Boolean {
                true_count,
                false_count,
            } => {
                assert_eq!(*true_count, 3);
                assert_eq!(*false_count, 2);
            }
            _ => panic!("expected boolean stats"),
        }
    }

    #[test]
    fn summarize_record_batches_empty() {
        let summary = summarize_record_batches(&[]).unwrap();
        assert_eq!(summary.num_rows, 0);
        assert_eq!(summary.columns.len(), 0);
    }
}
