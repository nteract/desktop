use arrow::array::{
    Array, BooleanArray, Float64Array, Int32Array, Int64Array, LargeStringArray, StringArray,
    UInt32Array, UInt64Array,
};
use arrow::datatypes::{DataType, TimeUnit};
use arrow::ipc::reader::StreamReader;
use arrow::ipc::writer::StreamWriter;
use arrow::record_batch::RecordBatch;
use arrow_cast::display::ArrayFormatter;
use arrow_ord::sort::{sort_to_indices, SortOptions};
use arrow_select::concat::concat;
use nteract_predicate::summary::{CategoryCount, HistogramBin};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::sync::Mutex;
use wasm_bindgen::prelude::*;

/// A loaded dataset stored in WASM memory.
struct DataStore {
    batches: Vec<RecordBatch>,
    /// Prefix sum of batch row counts for O(log n) row→batch lookup
    batch_offsets: Vec<usize>,
    total_rows: usize,
    num_cols: usize,
    col_names: Vec<String>,
    col_types: Vec<String>, // "numeric", "categorical", "boolean", "timestamp"
    /// Original column arrays saved before casting, keyed by column index.
    /// Used to restore original data when casting back to the original type.
    original_columns: HashMap<usize, (Vec<arrow::array::ArrayRef>, String)>,
}

impl DataStore {
    fn resolve_row(&self, row: usize) -> Option<(usize, usize)> {
        if row >= self.total_rows {
            return None;
        }
        // Binary search for the batch containing this row
        let batch_idx = match self.batch_offsets.binary_search(&row) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let local_row = row - self.batch_offsets[batch_idx];
        Some((batch_idx, local_row))
    }

    fn detect_col_type(dt: &DataType) -> &'static str {
        match dt {
            DataType::Boolean => "boolean",
            DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::UInt8
            | DataType::UInt16
            | DataType::UInt32
            | DataType::UInt64
            | DataType::Float16
            | DataType::Float32
            | DataType::Float64
            | DataType::Decimal128(_, _)
            | DataType::Decimal256(_, _) => "numeric",
            DataType::Timestamp(_, _) | DataType::Date32 | DataType::Date64 => "timestamp",
            _ => "categorical",
        }
    }
}

// Global store: handle → DataStore
static STORES: Mutex<Option<HashMap<u32, DataStore>>> = Mutex::new(None);
static NEXT_HANDLE: Mutex<u32> = Mutex::new(1);

fn with_stores<F, R>(f: F) -> R
where
    F: FnOnce(&mut HashMap<u32, DataStore>) -> R,
{
    let mut guard = STORES.lock().unwrap();
    let stores = guard.get_or_insert_with(HashMap::new);
    f(stores)
}

fn with_store<F, R>(handle: u32, f: F) -> Result<R, String>
where
    F: FnOnce(&DataStore) -> R,
{
    with_stores(|stores| {
        stores
            .get(&handle)
            .map(f)
            .ok_or_else(|| format!("Invalid handle: {}", handle))
    })
}

/// Store a vec of RecordBatches, returning a handle.
fn store_batches(batches: Vec<RecordBatch>, schema: &arrow::datatypes::Schema) -> u32 {
    let num_cols = schema.fields().len();
    let col_names: Vec<String> = schema.fields().iter().map(|f| f.name().clone()).collect();
    let col_types: Vec<String> = schema
        .fields()
        .iter()
        .map(|f| DataStore::detect_col_type(f.data_type()).to_string())
        .collect();

    let mut batch_offsets = Vec::new();
    let mut total_rows = 0;
    for batch in &batches {
        batch_offsets.push(total_rows);
        total_rows += batch.num_rows();
    }

    let handle = {
        let mut h = NEXT_HANDLE.lock().unwrap();
        let id = *h;
        *h += 1;
        id
    };

    with_stores(|stores| {
        stores.insert(
            handle,
            DataStore {
                batches,
                batch_offsets,
                total_rows,
                num_cols,
                col_names,
                col_types,
                original_columns: HashMap::new(),
            },
        );
    });

    handle
}

/// Load Arrow IPC bytes into WASM memory. Returns a handle for subsequent operations.
#[wasm_bindgen]
pub fn load_ipc(ipc_bytes: &[u8]) -> Result<u32, JsValue> {
    let cursor = Cursor::new(ipc_bytes);
    let reader =
        StreamReader::try_new(cursor, None).map_err(|e| JsValue::from_str(&e.to_string()))?;
    let schema = reader.schema();

    let mut batches = Vec::new();
    for batch in reader {
        batches.push(batch.map_err(|e| JsValue::from_str(&e.to_string()))?);
    }

    Ok(store_batches(batches, &schema))
}

/// Load Parquet bytes into WASM memory. Returns a handle for subsequent operations.
/// This replaces the need for parquet-wasm — one WASM binary for everything.
#[wasm_bindgen]
pub fn load_parquet(parquet_bytes: &[u8]) -> Result<u32, JsValue> {
    let bytes = bytes::Bytes::copy_from_slice(parquet_bytes);
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes)
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    let schema = builder.schema().clone();
    let reader = builder
        .build()
        .map_err(|e| JsValue::from_str(&e.to_string()))?;

    let mut batches = Vec::new();
    for batch in reader {
        batches.push(batch.map_err(|e| JsValue::from_str(&e.to_string()))?);
    }

    Ok(store_batches(batches, &schema))
}

/// Free a loaded dataset from WASM memory.
#[wasm_bindgen]
pub fn free(handle: u32) {
    with_stores(|stores| {
        stores.remove(&handle);
    });
}

/// Get the number of rows in a loaded dataset.
#[wasm_bindgen]
pub fn num_rows(handle: u32) -> Result<u32, JsValue> {
    with_store(handle, |s| s.total_rows as u32).map_err(|e| JsValue::from_str(&e))
}

/// Get the number of columns in a loaded dataset.
#[wasm_bindgen]
pub fn num_cols(handle: u32) -> Result<u32, JsValue> {
    with_store(handle, |s| s.num_cols as u32).map_err(|e| JsValue::from_str(&e))
}

/// Get column names as a JSON array.
#[wasm_bindgen]
pub fn col_names(handle: u32) -> Result<JsValue, JsValue> {
    with_store(handle, |s| {
        // Returns Vec<String> — trivial serialization
        serde_wasm_bindgen::to_value(&s.col_names).unwrap_or(JsValue::NULL)
    })
    .map_err(|e| JsValue::from_str(&e))
}

/// Get the detected type of a column ("numeric", "categorical", "boolean", "timestamp").
#[wasm_bindgen]
pub fn col_type(handle: u32, col: usize) -> Result<String, JsValue> {
    with_store(handle, |s| {
        s.col_types.get(col).cloned().unwrap_or_default()
    })
    .map_err(|e| JsValue::from_str(&e))
}

/// Check if a cell is null.
#[wasm_bindgen]
pub fn is_null(handle: u32, row: usize, col: usize) -> Result<bool, JsValue> {
    with_store(handle, |s| {
        let (batch_idx, local_row) = s.resolve_row(row).unwrap_or((0, 0));
        let column = s.batches[batch_idx].column(col);
        column.is_null(local_row)
    })
    .map_err(|e| JsValue::from_str(&e))
}

/// Get a cell value as a formatted string (for display).
#[wasm_bindgen]
pub fn get_cell_string(handle: u32, row: usize, col: usize) -> Result<String, JsValue> {
    with_store(handle, |s| {
        let (batch_idx, local_row) = match s.resolve_row(row) {
            Some(r) => r,
            None => return String::new(),
        };
        let column = s.batches[batch_idx].column(col);
        if column.is_null(local_row) {
            return String::new();
        }

        // Strings (Utf8 / LargeUtf8 / Utf8View / Dict<string>) route through
        // the shared helper so all variants dispatch correctly.
        if let Some(s) = nteract_predicate::arrow_utils::string_at(column.as_ref(), local_row) {
            return s;
        }

        match column.data_type() {
            DataType::Boolean => column
                .as_any()
                .downcast_ref::<BooleanArray>()
                .map(|a| {
                    if a.value(local_row) {
                        "Yes".into()
                    } else {
                        "No".into()
                    }
                })
                .unwrap_or_default(),
            DataType::Int32 => column
                .as_any()
                .downcast_ref::<Int32Array>()
                .map(|a| a.value(local_row).to_string())
                .unwrap_or_default(),
            DataType::Int64 => column
                .as_any()
                .downcast_ref::<Int64Array>()
                .map(|a| a.value(local_row).to_string())
                .unwrap_or_default(),
            DataType::Float64 => column
                .as_any()
                .downcast_ref::<Float64Array>()
                .map(|a| format!("{}", a.value(local_row)))
                .unwrap_or_default(),
            _ => ArrayFormatter::try_new(column.as_ref(), &Default::default())
                .ok()
                .map(|f| f.value(local_row).to_string())
                .unwrap_or_default(),
        }
    })
    .map_err(|e| JsValue::from_str(&e))
}

/// Get a cell value as f64 (for numeric sorting/comparison). Returns NaN for non-numeric or null.
#[wasm_bindgen]
pub fn get_cell_f64(handle: u32, row: usize, col: usize) -> Result<f64, JsValue> {
    with_store(handle, |s| {
        let (batch_idx, local_row) = match s.resolve_row(row) {
            Some(r) => r,
            None => return f64::NAN,
        };
        let column = s.batches[batch_idx].column(col);
        if column.is_null(local_row) {
            return f64::NAN;
        }

        match column.data_type() {
            DataType::Float64 => column
                .as_any()
                .downcast_ref::<Float64Array>()
                .map(|a| a.value(local_row))
                .unwrap_or(f64::NAN),
            DataType::Int32 => column
                .as_any()
                .downcast_ref::<Int32Array>()
                .map(|a| a.value(local_row) as f64)
                .unwrap_or(f64::NAN),
            DataType::Int64 => column
                .as_any()
                .downcast_ref::<Int64Array>()
                .map(|a| a.value(local_row) as f64)
                .unwrap_or(f64::NAN),
            _ => f64::NAN,
        }
    })
    .map_err(|e| JsValue::from_str(&e))
}

/// Compute value_counts for a column in a loaded store. Much faster than
/// the JS accumulator path since it iterates batches in Rust.
#[wasm_bindgen]
pub fn store_value_counts(handle: u32, col: usize) -> Result<JsValue, JsValue> {
    with_store(handle, |s| {
        let mut freq: HashMap<String, u32> = HashMap::new();
        for batch in &s.batches {
            let column = batch.column(col);
            if nteract_predicate::arrow_utils::for_each_string(column.as_ref(), |s| {
                *freq.entry(s.to_string()).or_insert(0) += 1;
            }) {
                continue;
            }
            match column.data_type() {
                DataType::Boolean => {
                    if let Some(arr) = column.as_any().downcast_ref::<BooleanArray>() {
                        for i in 0..arr.len() {
                            if !arr.is_null(i) {
                                let key = if arr.value(i) { "Yes" } else { "No" };
                                *freq.entry(key.to_string()).or_insert(0) += 1;
                            }
                        }
                    }
                }
                _ => {
                    if let Ok(formatter) =
                        ArrayFormatter::try_new(column.as_ref(), &Default::default())
                    {
                        for i in 0..column.len() {
                            if !column.is_null(i) {
                                *freq.entry(formatter.value(i).to_string()).or_insert(0) += 1;
                            }
                        }
                    }
                }
            }
        }
        let mut counts: Vec<CategoryCount> = freq
            .into_iter()
            .map(|(label, count)| CategoryCount { label, count })
            .collect();
        counts.sort_by(|a, b| b.count.cmp(&a.count));
        // Returns Vec<CategoryCount> — simple structs with String/u32 fields
        serde_wasm_bindgen::to_value(&counts).unwrap_or(JsValue::NULL)
    })
    .map_err(|e| JsValue::from_str(&e))
}

/// Compute histogram for a numeric column in a loaded store.
#[wasm_bindgen]
pub fn store_histogram(handle: u32, col: usize, num_bins: usize) -> Result<JsValue, JsValue> {
    with_store(handle, |s| {
        let mut values: Vec<f64> = Vec::new();
        for batch in &s.batches {
            let column = batch.column(col);
            match column.data_type() {
                DataType::Float64 => {
                    if let Some(arr) = column.as_any().downcast_ref::<Float64Array>() {
                        for i in 0..arr.len() {
                            if !arr.is_null(i) {
                                let v = arr.value(i);
                                if v.is_finite() {
                                    values.push(v);
                                }
                            }
                        }
                    }
                }
                DataType::Int32 => {
                    if let Some(arr) = column.as_any().downcast_ref::<Int32Array>() {
                        for i in 0..arr.len() {
                            if !arr.is_null(i) {
                                values.push(arr.value(i) as f64);
                            }
                        }
                    }
                }
                DataType::Int64 => {
                    if let Some(arr) = column.as_any().downcast_ref::<Int64Array>() {
                        for i in 0..arr.len() {
                            if !arr.is_null(i) {
                                values.push(arr.value(i) as f64);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if values.is_empty() {
            // serde_wasm_bindgen serialization won't fail for simple structs
            return JsValue::from(js_sys::Array::new());
        }
        let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let bin_width = if (max - min).abs() < f64::EPSILON {
            1.0
        } else {
            (max - min) / num_bins as f64
        };
        let mut bins: Vec<HistogramBin> = (0..num_bins)
            .map(|i| HistogramBin {
                x0: min + i as f64 * bin_width,
                x1: min + (i + 1) as f64 * bin_width,
                count: 0,
            })
            .collect();
        for v in &values {
            let mut idx = ((v - min) / bin_width) as usize;
            if idx >= num_bins {
                idx = num_bins - 1;
            }
            if idx > 0 && *v < bins[idx].x0 {
                idx -= 1;
            } else if idx + 1 < num_bins && *v >= bins[idx + 1].x0 {
                idx += 1;
            }
            bins[idx].count += 1;
        }
        // Returns Vec<HistogramBin> — simple struct with f64/u32 fields
        serde_wasm_bindgen::to_value(&bins).unwrap_or(JsValue::NULL)
    })
    .map_err(|e| JsValue::from_str(&e))
}

/// Compute temporal histogram: bins timestamps by calendar unit (auto-detected).
/// Granularity: <48h → hourly, <90d → daily, <3y → monthly, else yearly.
/// Returns bins with x0/x1 as epoch milliseconds.
#[wasm_bindgen]
pub fn store_temporal_histogram(handle: u32, col: usize) -> Result<JsValue, JsValue> {
    with_store(handle, |s| {
        let mut ms_values: Vec<i64> = Vec::new();
        for batch in &s.batches {
            let column = batch.column(col);
            extract_timestamp_ms(column, &mut ms_values);
        }
        if ms_values.is_empty() {
            return JsValue::from(js_sys::Array::new());
        }

        let min_ms = *ms_values.iter().min().unwrap();
        let max_ms = *ms_values.iter().max().unwrap();
        let range_ms = max_ms - min_ms;

        // Auto-detect granularity
        let ms_per_hour: i64 = 3_600_000;
        let ms_per_day: i64 = 86_400_000;
        let ms_per_month: i64 = 30 * ms_per_day; // approximate
        let ms_per_year: i64 = 365 * ms_per_day;

        let bin_width_ms = if range_ms < 48 * ms_per_hour {
            ms_per_hour
        } else if range_ms < 90 * ms_per_day {
            ms_per_day
        } else if range_ms < 3 * ms_per_year {
            ms_per_month
        } else {
            ms_per_year
        };

        // Align start to bin boundary
        let start = (min_ms / bin_width_ms) * bin_width_ms;
        let end = ((max_ms / bin_width_ms) + 1) * bin_width_ms;
        let num_bins = ((end - start) / bin_width_ms) as usize;

        // Cap at 100 bins to avoid huge arrays
        let (actual_start, actual_width, actual_count) = if num_bins > 100 {
            let w = (end - start) / 100;
            (start, w, 100usize)
        } else {
            (start, bin_width_ms, num_bins)
        };

        let mut bins: Vec<HistogramBin> = (0..actual_count)
            .map(|i| HistogramBin {
                x0: (actual_start + i as i64 * actual_width) as f64,
                x1: (actual_start + (i as i64 + 1) * actual_width) as f64,
                count: 0,
            })
            .collect();

        for &v in &ms_values {
            let mut idx = ((v - actual_start) / actual_width) as usize;
            if idx >= actual_count {
                idx = actual_count - 1;
            }
            bins[idx].count += 1;
        }

        // Returns Vec<HistogramBin> — simple struct with f64/u32 fields
        serde_wasm_bindgen::to_value(&bins).unwrap_or(JsValue::NULL)
    })
    .map_err(|e| JsValue::from_str(&e))
}

/// Extract timestamp values as milliseconds from an Arrow column.
fn extract_timestamp_ms(column: &dyn Array, out: &mut Vec<i64>) {
    match column.data_type() {
        DataType::Timestamp(TimeUnit::Millisecond, _) => {
            if let Some(arr) = column
                .as_any()
                .downcast_ref::<arrow::array::TimestampMillisecondArray>()
            {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        out.push(arr.value(i));
                    }
                }
            }
        }
        DataType::Timestamp(TimeUnit::Microsecond, _) => {
            if let Some(arr) = column
                .as_any()
                .downcast_ref::<arrow::array::TimestampMicrosecondArray>()
            {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        out.push(arr.value(i) / 1000);
                    }
                }
            }
        }
        DataType::Timestamp(TimeUnit::Nanosecond, _) => {
            if let Some(arr) = column
                .as_any()
                .downcast_ref::<arrow::array::TimestampNanosecondArray>()
            {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        out.push(arr.value(i) / 1_000_000);
                    }
                }
            }
        }
        DataType::Timestamp(TimeUnit::Second, _) => {
            if let Some(arr) = column
                .as_any()
                .downcast_ref::<arrow::array::TimestampSecondArray>()
            {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        out.push(arr.value(i) * 1000);
                    }
                }
            }
        }
        DataType::Date32 => {
            if let Some(arr) = column.as_any().downcast_ref::<arrow::array::Date32Array>() {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        out.push(arr.value(i) as i64 * 86_400_000);
                    }
                }
            }
        }
        DataType::Int64 => {
            // Fallback: treat i64 as epoch ms (common for cast timestamps)
            if let Some(arr) = column.as_any().downcast_ref::<Int64Array>() {
                for i in 0..arr.len() {
                    if !arr.is_null(i) {
                        out.push(arr.value(i));
                    }
                }
            }
        }
        _ => {}
    }
}

/// Count boolean values in a column: returns [true_count, false_count, null_count].
#[wasm_bindgen]
pub fn store_bool_counts(handle: u32, col: usize) -> Result<Vec<u32>, JsValue> {
    with_store(handle, |s| {
        let mut true_count: u32 = 0;
        let mut false_count: u32 = 0;
        let mut null_count: u32 = 0;
        for batch in &s.batches {
            let column = batch.column(col);
            if let Some(arr) = column.as_any().downcast_ref::<BooleanArray>() {
                for i in 0..arr.len() {
                    if arr.is_null(i) {
                        null_count += 1;
                    } else if arr.value(i) {
                        true_count += 1;
                    } else {
                        false_count += 1;
                    }
                }
            }
        }
        vec![true_count, false_count, null_count]
    })
    .map_err(|e| JsValue::from_str(&e))
}

// --- Filtered summaries (crossfilter) ---
// These take a byte mask (one byte per row, 0 = excluded, nonzero = included).
// Iterates batches once, checks mask per row — no allocation of filtered copies.

/// Filtered histogram: computes bins only for rows where mask[row] != 0.
#[wasm_bindgen]
pub fn store_filtered_histogram(
    handle: u32,
    col: usize,
    mask: &[u8],
    num_bins: usize,
) -> Result<JsValue, JsValue> {
    with_store(handle, |s| {
        let mut values: Vec<f64> = Vec::new();
        let mut global_row: usize = 0;
        for batch in &s.batches {
            let column = batch.column(col);
            let n = column.len();
            match column.data_type() {
                DataType::Float64 => {
                    if let Some(arr) = column.as_any().downcast_ref::<Float64Array>() {
                        for i in 0..n {
                            if global_row + i < mask.len()
                                && mask[global_row + i] != 0
                                && !arr.is_null(i)
                            {
                                let v = arr.value(i);
                                if v.is_finite() {
                                    values.push(v);
                                }
                            }
                        }
                    }
                }
                DataType::Int32 => {
                    if let Some(arr) = column.as_any().downcast_ref::<Int32Array>() {
                        for i in 0..n {
                            if global_row + i < mask.len()
                                && mask[global_row + i] != 0
                                && !arr.is_null(i)
                            {
                                values.push(arr.value(i) as f64);
                            }
                        }
                    }
                }
                DataType::Int64 => {
                    if let Some(arr) = column.as_any().downcast_ref::<Int64Array>() {
                        for i in 0..n {
                            if global_row + i < mask.len()
                                && mask[global_row + i] != 0
                                && !arr.is_null(i)
                            {
                                values.push(arr.value(i) as f64);
                            }
                        }
                    }
                }
                DataType::Timestamp(unit, _) => {
                    // Normalize to milliseconds to match the unfiltered histogram
                    // (`extract_timestamp_ms`) and the frontend's Date-based
                    // rendering. Without this, polars' default Datetime(us) /
                    // nanosecond clickhouse columns / second-precision epoch
                    // sources all fall through the old MillisecondArray-only
                    // downcast, leave `values` empty, and make the filtered
                    // histogram silently fall back to the unfiltered summary —
                    // so the header range never zooms in on a brushed filter.
                    match unit {
                        TimeUnit::Second => {
                            if let Some(arr) = column
                                .as_any()
                                .downcast_ref::<arrow::array::TimestampSecondArray>()
                            {
                                for i in 0..n {
                                    if global_row + i < mask.len()
                                        && mask[global_row + i] != 0
                                        && !arr.is_null(i)
                                    {
                                        values.push((arr.value(i) as f64) * 1000.0);
                                    }
                                }
                            }
                        }
                        TimeUnit::Millisecond => {
                            if let Some(arr) = column
                                .as_any()
                                .downcast_ref::<arrow::array::TimestampMillisecondArray>(
                            ) {
                                for i in 0..n {
                                    if global_row + i < mask.len()
                                        && mask[global_row + i] != 0
                                        && !arr.is_null(i)
                                    {
                                        values.push(arr.value(i) as f64);
                                    }
                                }
                            }
                        }
                        TimeUnit::Microsecond => {
                            if let Some(arr) = column
                                .as_any()
                                .downcast_ref::<arrow::array::TimestampMicrosecondArray>(
                            ) {
                                for i in 0..n {
                                    if global_row + i < mask.len()
                                        && mask[global_row + i] != 0
                                        && !arr.is_null(i)
                                    {
                                        values.push((arr.value(i) / 1_000) as f64);
                                    }
                                }
                            }
                        }
                        TimeUnit::Nanosecond => {
                            if let Some(arr) = column
                                .as_any()
                                .downcast_ref::<arrow::array::TimestampNanosecondArray>()
                            {
                                for i in 0..n {
                                    if global_row + i < mask.len()
                                        && mask[global_row + i] != 0
                                        && !arr.is_null(i)
                                    {
                                        values.push((arr.value(i) / 1_000_000) as f64);
                                    }
                                }
                            }
                        }
                    }
                }
                DataType::Date32 => {
                    // Date32 is days since epoch. Normalize to ms to match the
                    // unfiltered path (`extract_timestamp_ms` does the same).
                    if let Some(arr) = column.as_any().downcast_ref::<arrow::array::Date32Array>() {
                        for i in 0..n {
                            if global_row + i < mask.len()
                                && mask[global_row + i] != 0
                                && !arr.is_null(i)
                            {
                                values.push((arr.value(i) as i64 * 86_400_000) as f64);
                            }
                        }
                    }
                }
                DataType::Date64 => {
                    // Date64 is already milliseconds since epoch.
                    if let Some(arr) = column.as_any().downcast_ref::<arrow::array::Date64Array>() {
                        for i in 0..n {
                            if global_row + i < mask.len()
                                && mask[global_row + i] != 0
                                && !arr.is_null(i)
                            {
                                values.push(arr.value(i) as f64);
                            }
                        }
                    }
                }
                _ => {}
            }
            global_row += n;
        }
        if values.is_empty() {
            return JsValue::from(js_sys::Array::new());
        }
        let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let bin_width = if (max - min).abs() < f64::EPSILON {
            1.0
        } else {
            (max - min) / num_bins as f64
        };
        let mut bins: Vec<HistogramBin> = (0..num_bins)
            .map(|i| HistogramBin {
                x0: min + i as f64 * bin_width,
                x1: min + (i + 1) as f64 * bin_width,
                count: 0,
            })
            .collect();
        for v in &values {
            let mut idx = ((v - min) / bin_width) as usize;
            if idx >= num_bins {
                idx = num_bins - 1;
            }
            if idx > 0 && *v < bins[idx].x0 {
                idx -= 1;
            } else if idx + 1 < num_bins && *v >= bins[idx + 1].x0 {
                idx += 1;
            }
            bins[idx].count += 1;
        }
        // Returns Vec<HistogramBin> — simple struct with f64/u32 fields
        serde_wasm_bindgen::to_value(&bins).unwrap_or(JsValue::NULL)
    })
    .map_err(|e| JsValue::from_str(&e))
}

/// Filtered value_counts: counts string values only for rows where mask[row] != 0.
#[wasm_bindgen]
pub fn store_filtered_value_counts(
    handle: u32,
    col: usize,
    mask: &[u8],
) -> Result<JsValue, JsValue> {
    with_store(handle, |s| {
        let mut freq: HashMap<String, u32> = HashMap::new();
        let mut global_row: usize = 0;
        for batch in &s.batches {
            let column = batch.column(col);
            let n = column.len();
            // String-like columns: iterate via shared helper, applying the
            // filter mask per row (global_row + i maps into the full mask).
            let applied =
                nteract_predicate::arrow_utils::for_each_string_indexed(column.as_ref(), |i, s| {
                    if global_row + i < mask.len() && mask[global_row + i] != 0 {
                        *freq.entry(s.to_string()).or_insert(0) += 1;
                    }
                });
            if applied {
                global_row += n;
                continue;
            }
            match column.data_type() {
                DataType::Boolean => {
                    if let Some(arr) = column.as_any().downcast_ref::<BooleanArray>() {
                        for i in 0..n {
                            if global_row + i < mask.len()
                                && mask[global_row + i] != 0
                                && !arr.is_null(i)
                            {
                                let key = if arr.value(i) { "Yes" } else { "No" };
                                *freq.entry(key.to_string()).or_insert(0) += 1;
                            }
                        }
                    }
                }
                _ => {
                    if let Ok(formatter) =
                        ArrayFormatter::try_new(column.as_ref(), &Default::default())
                    {
                        for i in 0..n {
                            if global_row + i < mask.len()
                                && mask[global_row + i] != 0
                                && !column.is_null(i)
                            {
                                *freq.entry(formatter.value(i).to_string()).or_insert(0) += 1;
                            }
                        }
                    }
                }
            }
            global_row += n;
        }
        let mut counts: Vec<CategoryCount> = freq
            .into_iter()
            .map(|(label, count)| CategoryCount { label, count })
            .collect();
        counts.sort_by(|a, b| b.count.cmp(&a.count));
        // Returns Vec<CategoryCount> or Vec<HistogramBin> — simple structs with String/f64/u32 fields
        serde_wasm_bindgen::to_value(&counts).unwrap_or(JsValue::NULL)
    })
    .map_err(|e| JsValue::from_str(&e))
}

/// Filtered bool counts: returns [true_count, false_count, null_count] for masked rows.
#[wasm_bindgen]
pub fn store_filtered_bool_counts(
    handle: u32,
    col: usize,
    mask: &[u8],
) -> Result<Vec<u32>, JsValue> {
    with_store(handle, |s| {
        let mut true_count: u32 = 0;
        let mut false_count: u32 = 0;
        let mut null_count: u32 = 0;
        let mut global_row: usize = 0;
        for batch in &s.batches {
            let column = batch.column(col);
            let n = column.len();
            if let Some(arr) = column.as_any().downcast_ref::<BooleanArray>() {
                for i in 0..n {
                    if global_row + i < mask.len() && mask[global_row + i] != 0 {
                        if arr.is_null(i) {
                            null_count += 1;
                        } else if arr.value(i) {
                            true_count += 1;
                        } else {
                            false_count += 1;
                        }
                    }
                }
            }
            global_row += n;
        }
        vec![true_count, false_count, null_count]
    })
    .map_err(|e| JsValue::from_str(&e))
}

/// Sort a column and return sorted row indices.
/// `ascending`: true for asc, false for desc.
/// Nulls are always sorted to the end.
#[wasm_bindgen]
pub fn store_sort_indices(handle: u32, col: usize, ascending: bool) -> Result<Vec<u32>, JsValue> {
    with_store(handle, |s| {
        // Concatenate column across all batches into a single array
        let arrays: Vec<&dyn Array> = s.batches.iter().map(|b| b.column(col).as_ref()).collect();

        if arrays.is_empty() {
            return Ok(Vec::new());
        }

        let combined = concat(&arrays).map_err(|e| format!("concat error: {}", e))?;

        let options = SortOptions {
            descending: !ascending,
            nulls_first: false, // nulls always at end
        };

        let indices = sort_to_indices(combined.as_ref(), Some(options), None)
            .map_err(|e| format!("sort error: {}", e))?;

        // Convert UInt32Array to Vec<u32>
        Ok(indices.values().iter().copied().collect())
    })
    .map_err(|e| JsValue::from_str(&e))?
    .map_err(|e: String| JsValue::from_str(&e))
}

/// Get a viewport slice as Arrow IPC bytes.
/// Returns the rows [start_row, end_row) serialized as Arrow IPC stream.
/// This is the hot-path function — one call per scroll frame.
#[wasm_bindgen]
pub fn get_viewport(handle: u32, start_row: u32, end_row: u32) -> Result<Vec<u8>, JsValue> {
    with_store(handle, |s| {
        let start = start_row as usize;
        let end = (end_row as usize).min(s.total_rows);
        if start >= end {
            return Err("empty viewport".to_string());
        }

        let schema = s.batches[0].schema();
        let mut slices: Vec<RecordBatch> = Vec::new();

        // Walk batches, slicing the ones that overlap [start, end)
        for (batch_idx, batch) in s.batches.iter().enumerate() {
            let batch_start = s.batch_offsets[batch_idx];
            let batch_end = batch_start + batch.num_rows();

            // Skip batches entirely before or after the viewport
            if batch_end <= start || batch_start >= end {
                continue;
            }

            // Compute the overlap
            let local_start = start.saturating_sub(batch_start);
            let local_end = if end < batch_end {
                end - batch_start
            } else {
                batch.num_rows()
            };

            slices.push(batch.slice(local_start, local_end - local_start));
        }

        if slices.is_empty() {
            return Err("no data in viewport".to_string());
        }

        // Serialize to Arrow IPC stream
        let mut buf = Vec::new();
        let mut writer = StreamWriter::try_new(&mut buf, &schema)
            .map_err(|e| format!("IPC writer error: {}", e))?;
        for slice in &slices {
            writer
                .write(slice)
                .map_err(|e| format!("IPC write error: {}", e))?;
        }
        writer
            .finish()
            .map_err(|e| format!("IPC finish error: {}", e))?;
        drop(writer);

        Ok(buf)
    })
    .map_err(|e| JsValue::from_str(&e))?
    .map_err(|e: String| JsValue::from_str(&e))
}

/// Get a viewport slice for specific rows by index (for sorted/filtered views).
/// `indices` is a Uint32Array of row indices to fetch.
/// Returns Arrow IPC bytes containing those specific rows in order.
#[wasm_bindgen]
pub fn get_viewport_by_indices(handle: u32, indices: &[u32]) -> Result<Vec<u8>, JsValue> {
    with_store(handle, |s| {
        if indices.is_empty() || s.batches.is_empty() {
            return Err("empty indices".to_string());
        }

        let schema = s.batches[0].schema();
        let num_cols = schema.fields().len();

        // For each column, gather values at the requested indices using arrow take
        let mut columns: Vec<arrow::array::ArrayRef> = Vec::with_capacity(num_cols);

        for col_idx in 0..num_cols {
            // Concatenate column across all batches
            let arrays: Vec<&dyn Array> = s
                .batches
                .iter()
                .map(|b| b.column(col_idx).as_ref())
                .collect();
            let combined = concat(&arrays).map_err(|e| format!("concat error: {}", e))?;

            // Build indices array
            let idx_array = UInt32Array::from(indices.to_vec());
            let taken = arrow_select::take::take(combined.as_ref(), &idx_array, None)
                .map_err(|e| format!("take error: {}", e))?;
            columns.push(taken);
        }

        let batch =
            RecordBatch::try_new(schema, columns).map_err(|e| format!("batch error: {}", e))?;

        let mut buf = Vec::new();
        let mut writer = StreamWriter::try_new(&mut buf, batch.schema_ref())
            .map_err(|e| format!("IPC writer error: {}", e))?;
        writer
            .write(&batch)
            .map_err(|e| format!("IPC write error: {}", e))?;
        writer
            .finish()
            .map_err(|e| format!("IPC finish error: {}", e))?;
        drop(writer);

        Ok(buf)
    })
    .map_err(|e| JsValue::from_str(&e))?
    .map_err(|e: String| JsValue::from_str(&e))
}

/// Get Parquet metadata: number of row groups and total rows.
/// Returns [num_row_groups, total_rows] as Vec<u32>.
#[wasm_bindgen]
pub fn parquet_metadata(parquet_bytes: &[u8]) -> Result<Vec<u32>, JsValue> {
    let bytes = bytes::Bytes::copy_from_slice(parquet_bytes);
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes)
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    let metadata = builder.metadata();
    let num_row_groups = metadata.num_row_groups() as u32;
    let total_rows = metadata.file_metadata().num_rows() as u32;
    Ok(vec![num_row_groups, total_rows])
}

/// Extract key-value metadata from a Parquet file's schema.
/// Returns a JSON object with metadata keys like "pandas", "huggingface", etc.
#[wasm_bindgen]
pub fn parquet_schema_metadata(parquet_bytes: &[u8]) -> Result<JsValue, JsValue> {
    let bytes = bytes::Bytes::copy_from_slice(parquet_bytes);
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes)
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    let schema = builder.schema();
    let metadata = schema.metadata();
    let map: HashMap<String, String> = metadata
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    serde_wasm_bindgen::to_value(&map).map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Load a single Parquet row group into a new or existing store.
/// If handle is 0, creates a new store and returns the handle.
/// If handle is non-zero, appends the row group to the existing store.
#[wasm_bindgen]
pub fn load_parquet_row_group(
    parquet_bytes: &[u8],
    row_group: usize,
    handle: u32,
) -> Result<u32, JsValue> {
    let bytes = bytes::Bytes::copy_from_slice(parquet_bytes);
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes)
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    let schema = builder.schema().clone();

    let reader = builder
        .with_row_groups(vec![row_group])
        .build()
        .map_err(|e| JsValue::from_str(&e.to_string()))?;

    let mut batches = Vec::new();
    for batch in reader {
        batches.push(batch.map_err(|e| JsValue::from_str(&e.to_string()))?);
    }

    if handle == 0 {
        // Create new store
        Ok(store_batches(batches, &schema))
    } else {
        // Append to existing store
        with_stores(|stores| {
            if let Some(store) = stores.get_mut(&handle) {
                for batch in batches {
                    store.batch_offsets.push(store.total_rows);
                    store.total_rows += batch.num_rows();
                    store.batches.push(batch);
                }
                Ok(handle)
            } else {
                Err(JsValue::from_str(&format!("Invalid handle: {}", handle)))
            }
        })
    }
}

/// Check if a column has been cast (i.e. original data is saved and can be restored).
#[wasm_bindgen]
pub fn has_original_column(handle: u32, col: usize) -> Result<bool, JsValue> {
    with_store(handle, |s| s.original_columns.contains_key(&col)).map_err(|e| JsValue::from_str(&e))
}

/// Undo a column cast, restoring the original column data and type.
/// Returns the original column type string (e.g. "categorical", "numeric").
#[wasm_bindgen]
pub fn undo_cast_column(handle: u32, col: usize) -> Result<String, JsValue> {
    with_stores(|stores| {
        let store = stores
            .get_mut(&handle)
            .ok_or_else(|| JsValue::from_str(&format!("Invalid handle: {}", handle)))?;

        let (original_cols, original_type) = store
            .original_columns
            .remove(&col)
            .ok_or_else(|| JsValue::from_str(&format!("Column {} has not been cast", col)))?;

        let mut new_batches = Vec::new();
        for (batch_idx, batch) in store.batches.iter().enumerate() {
            let mut columns: Vec<arrow::array::ArrayRef> = Vec::new();
            for i in 0..batch.num_columns() {
                if i == col {
                    columns.push(original_cols[batch_idx].clone());
                } else {
                    columns.push(batch.column(i).clone());
                }
            }
            let mut fields: Vec<arrow::datatypes::FieldRef> =
                batch.schema().fields().iter().cloned().collect();
            fields[col] = std::sync::Arc::new(arrow::datatypes::Field::new(
                fields[col].name(),
                original_cols[batch_idx].data_type().clone(),
                true,
            ));
            let new_schema = std::sync::Arc::new(arrow::datatypes::Schema::new(fields));
            new_batches.push(
                RecordBatch::try_new(new_schema, columns)
                    .map_err(|e| JsValue::from_str(&format!("Batch rebuild error: {}", e)))?,
            );
        }
        store.batches = new_batches;
        store.col_types[col] = original_type.clone();

        Ok(original_type)
    })
}

/// Cast a column to a different type in-place.
/// Supported casts: string→timestamp (parse ISO dates), string→numeric, etc.
/// Uses arrow-cast for type conversion. Updates the store's column type metadata.
/// Saves the original column data so it can be restored when casting back.
#[wasm_bindgen]
pub fn cast_column(handle: u32, col: usize, target_type: &str) -> Result<(), JsValue> {
    with_stores(|stores| {
        let store = stores
            .get_mut(&handle)
            .ok_or_else(|| JsValue::from_str(&format!("Invalid handle: {}", handle)))?;

        let target_dt = match target_type {
            "timestamp" => DataType::Timestamp(arrow::datatypes::TimeUnit::Millisecond, None),
            "numeric" => DataType::Float64,
            "boolean" => DataType::Boolean,
            "categorical" => DataType::Utf8,
            _ => {
                return Err(JsValue::from_str(&format!(
                    "Unknown target type: {}",
                    target_type
                )))
            }
        };

        // Check if we have saved originals for this column and the target matches
        if let Some((original_cols, original_type)) = store.original_columns.get(&col) {
            if target_type == original_type {
                // Restore original column data instead of arrow-casting
                let mut new_batches = Vec::new();
                for (batch_idx, batch) in store.batches.iter().enumerate() {
                    let mut columns: Vec<arrow::array::ArrayRef> = Vec::new();
                    for i in 0..batch.num_columns() {
                        if i == col {
                            columns.push(original_cols[batch_idx].clone());
                        } else {
                            columns.push(batch.column(i).clone());
                        }
                    }
                    let mut fields: Vec<arrow::datatypes::FieldRef> =
                        batch.schema().fields().iter().cloned().collect();
                    fields[col] = std::sync::Arc::new(arrow::datatypes::Field::new(
                        fields[col].name(),
                        original_cols[batch_idx].data_type().clone(),
                        true,
                    ));
                    let new_schema = std::sync::Arc::new(arrow::datatypes::Schema::new(fields));
                    new_batches.push(
                        RecordBatch::try_new(new_schema, columns).map_err(|e| {
                            JsValue::from_str(&format!("Batch rebuild error: {}", e))
                        })?,
                    );
                }
                store.batches = new_batches;
                store.col_types[col] = original_type.clone();
                store.original_columns.remove(&col);
                return Ok(());
            }
        }

        // Save original column data before casting (only if not already saved)
        if !store.original_columns.contains_key(&col) {
            let originals: Vec<arrow::array::ArrayRef> = store
                .batches
                .iter()
                .map(|b| b.column(col).clone())
                .collect();
            let original_type = store.col_types[col].clone();
            store
                .original_columns
                .insert(col, (originals, original_type));
        }

        // Cast the column in each batch
        let mut new_batches = Vec::new();
        for batch in &store.batches {
            let column = batch.column(col);
            let source_dt = column.data_type();

            let casted = if source_dt == &target_dt {
                column.clone()
            } else if target_type == "timestamp"
                && matches!(source_dt, DataType::Utf8 | DataType::LargeUtf8)
            {
                // String → Timestamp: parse ISO date strings manually.
                // Accept both StringArray (Utf8) and LargeStringArray (LargeUtf8).
                let str_arr: &dyn Array = match source_dt {
                    DataType::Utf8 => {
                        column
                            .as_any()
                            .downcast_ref::<StringArray>()
                            .ok_or_else(|| {
                                JsValue::from_str(
                                    "expected StringArray for Utf8 column during cast",
                                )
                            })? as &dyn Array
                    }
                    DataType::LargeUtf8 => column
                        .as_any()
                        .downcast_ref::<LargeStringArray>()
                        .ok_or_else(|| {
                            JsValue::from_str(
                                "expected LargeStringArray for LargeUtf8 column during cast",
                            )
                        })? as &dyn Array,
                    _ => unreachable!(),
                };
                let mut builder = arrow::array::TimestampMillisecondArray::builder(str_arr.len());
                let parse_value = |i: usize| -> Option<&str> {
                    if str_arr.is_null(i) {
                        return None;
                    }
                    if let Some(a) = str_arr.as_any().downcast_ref::<StringArray>() {
                        Some(a.value(i))
                    } else {
                        str_arr
                            .as_any()
                            .downcast_ref::<LargeStringArray>()
                            .map(|a| a.value(i))
                    }
                };
                for i in 0..str_arr.len() {
                    match parse_value(i) {
                        None => builder.append_null(),
                        Some(s) => {
                            if let Ok(dt) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
                                // and_hms_opt(0,0,0) only fails for invalid h/m/s, which are hardcoded valid
                                let ts = dt
                                    .and_hms_opt(0, 0, 0)
                                    .unwrap()
                                    .and_utc()
                                    .timestamp_millis();
                                builder.append_value(ts);
                            } else if let Ok(dt) =
                                chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
                            {
                                builder.append_value(dt.and_utc().timestamp_millis());
                            } else {
                                builder.append_null();
                            }
                        }
                    }
                }
                std::sync::Arc::new(builder.finish()) as arrow::array::ArrayRef
            } else {
                // Use arrow-cast for other conversions.
                // Wrap in catch_unwind because some casts panic instead of returning Err
                // (e.g., casting text with non-numeric values to Float64).
                let col_ref = column.clone();
                let dt = target_dt.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    arrow_cast::cast::cast(col_ref.as_ref(), &dt)
                }));
                match result {
                    Ok(Ok(arr)) => arr,
                    Ok(Err(e)) => return Err(JsValue::from_str(&format!("Cast error: {}", e))),
                    Err(_) => {
                        return Err(JsValue::from_str(
                            "Cast failed: incompatible data for target type",
                        ))
                    }
                }
            };

            // Rebuild the batch with the casted column
            let mut columns: Vec<arrow::array::ArrayRef> = Vec::new();
            for i in 0..batch.num_columns() {
                if i == col {
                    columns.push(casted.clone());
                } else {
                    columns.push(batch.column(i).clone());
                }
            }

            // Update schema for this column
            let mut fields: Vec<arrow::datatypes::FieldRef> =
                batch.schema().fields().iter().cloned().collect();
            fields[col] = std::sync::Arc::new(arrow::datatypes::Field::new(
                fields[col].name(),
                target_dt.clone(),
                true,
            ));
            let new_schema = std::sync::Arc::new(arrow::datatypes::Schema::new(fields));
            new_batches.push(
                RecordBatch::try_new(new_schema, columns)
                    .map_err(|e| JsValue::from_str(&format!("Batch rebuild error: {}", e)))?,
            );
        }

        store.batches = new_batches;
        store.col_types[col] = match target_type {
            "timestamp" => "timestamp".to_string(),
            "numeric" => "numeric".to_string(),
            "boolean" => "boolean".to_string(),
            _ => "categorical".to_string(),
        };

        Ok(())
    })
}

// --- Store-based filter rows ---

#[derive(Deserialize)]
#[serde(tag = "kind")]
enum FilterSpec {
    #[serde(rename = "range")]
    Range { col: usize, min: f64, max: f64 },
    #[serde(rename = "set")]
    Set { col: usize, values: Vec<String> },
    #[serde(rename = "boolean")]
    Boolean { col: usize, value: bool },
}

/// Apply filter predicates to the store and return matching row indices.
/// `filters_js` is a JSON array of filter specs:
///   [{kind: "range", col: 0, min: 10, max: 50},
///    {kind: "set", col: 1, values: ["a", "b"]},
///    {kind: "boolean", col: 3, value: true}]
/// Returns a Vec<u32> of row indices that pass ALL filters (AND logic).
#[wasm_bindgen]
pub fn store_filter_rows(handle: u32, filters_js: JsValue) -> Result<Vec<u32>, JsValue> {
    let filters: Vec<FilterSpec> = serde_wasm_bindgen::from_value(filters_js)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse filters: {}", e)))?;

    if filters.is_empty() {
        // No filters — return all row indices
        return with_store(handle, |s| (0..s.total_rows as u32).collect())
            .map_err(|e| JsValue::from_str(&e));
    }

    // Pre-build HashSets for set filters
    let set_lookups: Vec<Option<HashSet<&str>>> = filters
        .iter()
        .map(|f| match f {
            FilterSpec::Set { values, .. } => Some(values.iter().map(|s| s.as_str()).collect()),
            _ => None,
        })
        .collect();

    with_store(handle, |s| {
        // Concatenate each filtered column once (avoids per-row batch resolution)
        let mut concat_cols: Vec<Option<arrow::array::ArrayRef>> = vec![None; s.num_cols];
        for filter in &filters {
            let col_idx = match filter {
                FilterSpec::Range { col, .. } => *col,
                FilterSpec::Set { col, .. } => *col,
                FilterSpec::Boolean { col, .. } => *col,
            };
            if concat_cols[col_idx].is_none() {
                let arrays: Vec<&dyn Array> = s
                    .batches
                    .iter()
                    .map(|b| b.column(col_idx).as_ref())
                    .collect();
                if !arrays.is_empty() {
                    if let Ok(combined) = concat(&arrays) {
                        concat_cols[col_idx] = Some(combined);
                    }
                }
            }
        }

        let total = s.total_rows;
        let mut result = Vec::with_capacity(total);

        'row: for row in 0..total {
            for (fi, filter) in filters.iter().enumerate() {
                match filter {
                    FilterSpec::Range { col, min, max } => {
                        if let Some(ref arr) = concat_cols[*col] {
                            if arr.is_null(row) {
                                continue 'row;
                            }
                            let v = get_f64_value(arr.as_ref(), row);
                            if v.is_nan() || v < *min || v > *max {
                                continue 'row;
                            }
                        }
                    }
                    FilterSpec::Set { col, .. } => {
                        if let Some(ref arr) = concat_cols[*col] {
                            let s = get_string_value(arr.as_ref(), row);
                            if let Some(ref lookup) = set_lookups[fi] {
                                if !lookup.contains(s.as_str()) {
                                    continue 'row;
                                }
                            }
                        }
                    }
                    FilterSpec::Boolean { col, value } => {
                        if let Some(ref arr) = concat_cols[*col] {
                            if arr.is_null(row) {
                                continue 'row;
                            }
                            if let Some(bool_arr) = arr.as_any().downcast_ref::<BooleanArray>() {
                                if bool_arr.value(row) != *value {
                                    continue 'row;
                                }
                            }
                        }
                    }
                }
            }
            result.push(row as u32);
        }
        result
    })
    .map_err(|e| JsValue::from_str(&e))
}

/// Extract an f64 from any numeric or timestamp array at the given row.
fn get_f64_value(arr: &dyn Array, row: usize) -> f64 {
    match arr.data_type() {
        DataType::Float64 => arr
            .as_any()
            .downcast_ref::<Float64Array>()
            .map(|a| a.value(row))
            .unwrap_or(f64::NAN),
        DataType::Float32 => arr
            .as_any()
            .downcast_ref::<arrow::array::Float32Array>()
            .map(|a| a.value(row) as f64)
            .unwrap_or(f64::NAN),
        DataType::Int32 => arr
            .as_any()
            .downcast_ref::<Int32Array>()
            .map(|a| a.value(row) as f64)
            .unwrap_or(f64::NAN),
        DataType::Int64 => arr
            .as_any()
            .downcast_ref::<Int64Array>()
            .map(|a| a.value(row) as f64)
            .unwrap_or(f64::NAN),
        DataType::UInt32 => arr
            .as_any()
            .downcast_ref::<UInt32Array>()
            .map(|a| a.value(row) as f64)
            .unwrap_or(f64::NAN),
        DataType::UInt64 => arr
            .as_any()
            .downcast_ref::<UInt64Array>()
            .map(|a| a.value(row) as f64)
            .unwrap_or(f64::NAN),
        DataType::Int16 => arr
            .as_any()
            .downcast_ref::<arrow::array::Int16Array>()
            .map(|a| a.value(row) as f64)
            .unwrap_or(f64::NAN),
        DataType::Int8 => arr
            .as_any()
            .downcast_ref::<arrow::array::Int8Array>()
            .map(|a| a.value(row) as f64)
            .unwrap_or(f64::NAN),
        DataType::UInt16 => arr
            .as_any()
            .downcast_ref::<arrow::array::UInt16Array>()
            .map(|a| a.value(row) as f64)
            .unwrap_or(f64::NAN),
        DataType::UInt8 => arr
            .as_any()
            .downcast_ref::<arrow::array::UInt8Array>()
            .map(|a| a.value(row) as f64)
            .unwrap_or(f64::NAN),
        DataType::Timestamp(TimeUnit::Millisecond, _) => arr
            .as_any()
            .downcast_ref::<arrow::array::TimestampMillisecondArray>()
            .map(|a| a.value(row) as f64)
            .unwrap_or(f64::NAN),
        DataType::Timestamp(TimeUnit::Microsecond, _) => arr
            .as_any()
            .downcast_ref::<arrow::array::TimestampMicrosecondArray>()
            .map(|a| a.value(row) as f64 / 1000.0)
            .unwrap_or(f64::NAN),
        DataType::Timestamp(TimeUnit::Nanosecond, _) => arr
            .as_any()
            .downcast_ref::<arrow::array::TimestampNanosecondArray>()
            .map(|a| a.value(row) as f64 / 1_000_000.0)
            .unwrap_or(f64::NAN),
        DataType::Timestamp(TimeUnit::Second, _) => arr
            .as_any()
            .downcast_ref::<arrow::array::TimestampSecondArray>()
            .map(|a| a.value(row) as f64 * 1000.0)
            .unwrap_or(f64::NAN),
        DataType::Date32 => arr
            .as_any()
            .downcast_ref::<arrow::array::Date32Array>()
            .map(|a| a.value(row) as f64 * 86_400_000.0)
            .unwrap_or(f64::NAN),
        DataType::Date64 => arr
            .as_any()
            .downcast_ref::<arrow::array::Date64Array>()
            .map(|a| a.value(row) as f64)
            .unwrap_or(f64::NAN),
        _ => f64::NAN,
    }
}

/// Extract a string value from any string, boolean, or dictionary-encoded column.
fn get_string_value(arr: &dyn Array, row: usize) -> String {
    // String-like types (Utf8 / LargeUtf8 / Utf8View / Dict<string>) via the
    // shared helper. Handles null internally.
    if let Some(s) = nteract_predicate::arrow_utils::string_at(arr, row) {
        return s;
    }
    if arr.is_null(row) {
        return String::new();
    }
    match arr.data_type() {
        DataType::Boolean => arr
            .as_any()
            .downcast_ref::<BooleanArray>()
            .map(|a| {
                if a.value(row) {
                    "Yes".to_string()
                } else {
                    "No".to_string()
                }
            })
            .unwrap_or_default(),
        _ => format!("{:?}", arr.as_any()),
    }
}
