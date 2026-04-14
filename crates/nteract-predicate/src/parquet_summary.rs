//! Parquet summarization for LLM text representations.
//!
//! Reads Parquet bytes and produces a structured summary: row count,
//! column types, per-column stats (null count, min/max for numerics,
//! top values for strings, true/false counts for booleans).
//!
//! Designed to be rendered as `text/llm+plain` so agents can understand
//! a dataset without rendering it.

use arrow::array::{
    Array, AsArray, BooleanArray, Date32Array, Date64Array, Decimal128Array, Decimal256Array,
    Float32Array, Float64Array, Int16Array, Int32Array, Int64Array, Int8Array, LargeStringArray,
    StringArray, TimestampMicrosecondArray, TimestampMillisecondArray, TimestampNanosecondArray,
    TimestampSecondArray, UInt16Array, UInt32Array, UInt64Array, UInt8Array,
};
use arrow::datatypes::{DataType, TimeUnit};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::Serialize;
use std::collections::HashMap;

use crate::utils::dict_key_at;

/// Maximum number of distinct values to enumerate for categorical columns.
const TOP_N_CATEGORIES: usize = 5;

/// Cap on distinct string values kept per column. Once a column's accumulator
/// reaches this size, new distinct values are dropped (existing values still
/// increment). Prevents OOM on high-cardinality columns (e.g., UUIDs, free text).
const MAX_DISTINCT_STRINGS_PER_COLUMN: usize = 10_000;

/// Maximum length of a single string value kept in the accumulator. Values
/// longer than this are truncated for stats only (does not affect the parquet
/// file itself). Protects against blob-in-a-column cases.
const MAX_STRING_VALUE_LEN: usize = 256;

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
        /// True if the accumulator hit its per-column cardinality cap during
        /// ingestion. When set, `distinct_count` is a lower bound and some
        /// rare values may be absent from `top`.
        distinct_count_capped: bool,
        top: Vec<(String, u64)>,
    },
    /// Temporal: min/max formatted as ISO strings.
    Temporal { min: String, max: String },
    /// Types we don't summarize (structs, lists, maps, etc).
    Other,
}

/// Summarize a Parquet file from its raw bytes.
///
/// Currently this does a full scan through all row groups. A future optimization
/// could read just the file footer (`parquet::file::reader::FileReader::metadata`)
/// for cheap stats (row count, per-column min/max/null_count via `Statistics`)
/// and fall back to full scan only when values need to be enumerated (top-N,
/// distinct count). This matters when the daemon needs to summarize large
/// (~100MB+) parquet outputs that might otherwise take seconds to scan.
pub fn summarize_parquet(
    bytes: &[u8],
) -> Result<ParquetSummary, Box<dyn std::error::Error + Send + Sync>> {
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

/// Increment the count for a string value in a column's accumulator, bounded
/// by [`MAX_DISTINCT_STRINGS_PER_COLUMN`] and [`MAX_STRING_VALUE_LEN`].
/// Once the accumulator is full, new distinct values are dropped but existing
/// entries continue to accumulate counts.
fn bump_string(acc: &mut HashMap<String, u64>, raw: &str) {
    // Truncate on char boundary to stay within byte cap.
    let val: &str = if raw.len() > MAX_STRING_VALUE_LEN {
        let mut end = MAX_STRING_VALUE_LEN;
        while end > 0 && !raw.is_char_boundary(end) {
            end -= 1;
        }
        &raw[..end]
    } else {
        raw
    };
    match acc.get_mut(val) {
        Some(v) => *v += 1,
        None => {
            if acc.len() < MAX_DISTINCT_STRINGS_PER_COLUMN {
                acc.insert(val.to_string(), 1);
            }
            // else: cardinality cap reached — drop this distinct value but
            // continue counting future occurrences of already-seen values.
        }
    }
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
        DataType::Decimal128(_, scale) => {
            if let Some(arr) = col.as_any().downcast_ref::<Decimal128Array>() {
                let divisor = 10f64.powi(*scale as i32);
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        update_numeric(&mut numeric_accum[col_idx], arr.value(i) as f64 / divisor);
                    }
                }
            }
        }
        DataType::Decimal256(_, scale) => {
            if let Some(arr) = col.as_any().downcast_ref::<Decimal256Array>() {
                let divisor = 10f64.powi(*scale as i32);
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        // i256 → f64 via Display; lossy for very large values but
                        // fine for summary min/max bounds.
                        if let Ok(v) = arr.value(i).to_string().parse::<f64>() {
                            update_numeric(&mut numeric_accum[col_idx], v / divisor);
                        }
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
                        bump_string(&mut string_accum[col_idx], arr.value(i));
                    }
                }
            }
        }
        DataType::LargeUtf8 => {
            if let Some(arr) = col.as_any().downcast_ref::<LargeStringArray>() {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        bump_string(&mut string_accum[col_idx], arr.value(i));
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
                        bump_string(&mut string_accum[col_idx], str_values.value(key));
                    }
                }
            } else if let Some(str_values) = values.as_any().downcast_ref::<LargeStringArray>() {
                for i in 0..keys.len() {
                    if let Some(key) = dict_key_at(keys, i) {
                        bump_string(&mut string_accum[col_idx], str_values.value(key));
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
        | DataType::UInt64
        | DataType::Decimal128(_, _)
        | DataType::Decimal256(_, _) => match numeric {
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
            // If the accumulator is saturated, we stopped admitting new distinct
            // values partway through. Flag this so callers can note "≥N distinct".
            let distinct_count_capped = string_counts.len() >= MAX_DISTINCT_STRINGS_PER_COLUMN;
            ColumnStats::String {
                distinct_count: string_counts.len() as u64,
                distinct_count_capped,
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
        DataType::Decimal128(p, s) => format!("decimal128({p},{s})"),
        DataType::Decimal256(p, s) => format!("decimal256({p},{s})"),
        DataType::Binary | DataType::LargeBinary => "binary".to_string(),
        DataType::FixedSizeBinary(n) => format!("binary[{n}]"),
        DataType::List(f) | DataType::LargeList(f) => {
            format!("list<{}>", format_data_type(f.data_type()))
        }
        DataType::FixedSizeList(f, n) => {
            format!("list<{}>[{n}]", format_data_type(f.data_type()))
        }
        DataType::Struct(fields) => format!("struct<{} fields>", fields.len()),
        DataType::Map(_, _) => "map".to_string(),
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
                distinct_count_capped,
                top,
            } => {
                assert_eq!(*distinct_count, 3);
                assert!(!distinct_count_capped);
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
    fn summarize_parquet_empty_ok() {
        // Zero batches = error (parquet file needs at least a schema), but
        // a schema-only parquet should produce 0 rows with column metadata.
        let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, true)]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Int64Array::from(Vec::<Option<i64>>::new()))],
        )
        .unwrap();
        let bytes = batch_to_parquet_bytes(&batch);
        let summary = summarize_parquet(&bytes).unwrap();
        assert_eq!(summary.num_rows, 0);
        assert_eq!(summary.columns.len(), 1);
        assert_eq!(summary.columns[0].name, "x");
    }

    #[test]
    fn string_cardinality_cap() {
        // More than MAX_DISTINCT_STRINGS_PER_COLUMN distinct values: the cap
        // kicks in and we retain only the first N we saw.
        let mut values: Vec<Option<&str>> = Vec::new();
        let strings: Vec<String> = (0..(MAX_DISTINCT_STRINGS_PER_COLUMN + 100))
            .map(|i| format!("v{i}"))
            .collect();
        for s in &strings {
            values.push(Some(s.as_str()));
        }
        let schema = Arc::new(Schema::new(vec![Field::new("s", DataType::Utf8, true)]));
        let arr = StringArray::from(values);
        let batch = RecordBatch::try_new(schema, vec![Arc::new(arr)]).unwrap();
        let bytes = batch_to_parquet_bytes(&batch);
        let summary = summarize_parquet(&bytes).unwrap();
        match &summary.columns[0].stats {
            ColumnStats::String {
                distinct_count,
                distinct_count_capped,
                ..
            } => {
                assert!(
                    *distinct_count <= MAX_DISTINCT_STRINGS_PER_COLUMN as u64,
                    "cap exceeded: {}",
                    distinct_count
                );
                assert!(*distinct_count_capped, "should be flagged as capped");
            }
            _ => panic!("expected string stats"),
        }
    }

    #[test]
    fn string_value_truncation() {
        // A string longer than MAX_STRING_VALUE_LEN is truncated in the accumulator.
        let long = "x".repeat(MAX_STRING_VALUE_LEN * 2);
        let schema = Arc::new(Schema::new(vec![Field::new("s", DataType::Utf8, false)]));
        let arr = StringArray::from(vec![long.as_str()]);
        let batch = RecordBatch::try_new(schema, vec![Arc::new(arr)]).unwrap();
        let bytes = batch_to_parquet_bytes(&batch);
        let summary = summarize_parquet(&bytes).unwrap();
        match &summary.columns[0].stats {
            ColumnStats::String { top, .. } => {
                assert_eq!(top[0].0.len(), MAX_STRING_VALUE_LEN);
            }
            _ => panic!("expected string stats"),
        }
    }
}
