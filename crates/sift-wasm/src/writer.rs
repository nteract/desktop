//! Parquet writer exports. Two shapes:
//!
//! - `write_parquet_ipc` — Arrow IPC bytes → ZSTD Parquet bytes. One row group
//!   per input RecordBatch.
//! - `write_parquet_from_sqlite` — SQLite `PRAGMA table_info(...)` rows +
//!   `SELECT * FROM ...` rows → ZSTD Parquet bytes. Arrow is built inside so
//!   callers don't have to pull `apache-arrow` in JS just to hand us IPC.
//!
//! Host-agnostic — runs anywhere `wasm-bindgen` runs (Cloudflare Workers,
//! browsers, Deno, Node).

use std::io::Cursor;
use std::sync::Arc;

use arrow::array::builder::{
    BinaryBuilder, BooleanBuilder, Date32Builder, Float64Builder, Int64Builder, StringBuilder,
    TimestampMicrosecondBuilder, TimestampMillisecondBuilder, TimestampNanosecondBuilder,
};
use arrow::array::{ArrayRef, RecordBatch};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::ipc::reader::StreamReader;
use chrono::{DateTime, NaiveDate, NaiveDateTime};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use serde::Deserialize;
use wasm_bindgen::prelude::*;

/// Write an Arrow IPC stream to a Parquet file. ZSTD compressed,
/// one row group per input RecordBatch.
#[wasm_bindgen]
pub fn write_parquet_ipc(ipc_bytes: &[u8]) -> Result<Vec<u8>, JsError> {
    let reader = StreamReader::try_new(Cursor::new(ipc_bytes), None)
        .map_err(|e| JsError::new(&format!("ipc reader: {e}")))?;
    let schema = reader.schema();
    let batches: Vec<RecordBatch> = reader
        .collect::<Result<_, _>>()
        .map_err(|e| JsError::new(&format!("ipc read batch: {e}")))?;

    write_batches_to_parquet(schema, batches)
}

/// Row shape returned by SQLite's `PRAGMA table_info(table)`:
/// one object per column with `name` (string) and `type` (SQLite type text).
/// Extra fields (notnull, pk, dflt_value, cid) are ignored.
#[derive(Deserialize)]
struct PragmaColumn {
    name: String,
    #[serde(rename = "type")]
    ty: String,
}

/// Semantic classification of a SQLite declared-type string.
///
/// SQLite intentionally has no canonical enum for declared types — any string
/// is legal and the engine uses substring matching to pick storage affinity.
/// We define our own so the mapping is testable and extensible.
///
/// Pass 1 variants (Boolean through Uuid) correspond to richer types that
/// SQLite collapses into coarse affinities but which we want to carry through
/// into Arrow/Parquet verbatim. Pass 2 variants (IntegerAffinity through
/// NumericAffinity) mirror SQLite's native affinity algorithm at
/// <https://www.sqlite.org/datatype3.html#determination_of_column_affinity>.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteDeclared {
    Boolean,
    TimestampSecond,
    TimestampMilli,
    TimestampMicro,
    TimestampNano,
    Date,
    Json,
    Uuid,
    IntegerAffinity,
    TextAffinity,
    BlobAffinity,
    RealAffinity,
    NumericAffinity,
}

impl SqliteDeclared {
    /// Parse a declared type string (as it appears in `PRAGMA table_info(...)`)
    /// into a semantic `SqliteDeclared`. Case-insensitive. Parameterization
    /// like `VARCHAR(100)` or `DECIMAL(10,2)` is stripped before matching so
    /// the base type resolves.
    pub fn parse(declared: &str) -> Self {
        let t = declared.trim().to_uppercase();
        let base = match t.find('(') {
            Some(i) => t[..i].trim().to_string(),
            None => t.clone(),
        };

        match base.as_str() {
            "BOOLEAN" | "BOOL" | "BIT" => return Self::Boolean,
            "TIMESTAMP" | "TIMESTAMP_S" | "TIMESTAMPTZ" => return Self::TimestampSecond,
            "TIMESTAMP_MS" | "DATETIME" => return Self::TimestampMilli,
            "TIMESTAMP_US" => return Self::TimestampMicro,
            "TIMESTAMP_NS" => return Self::TimestampNano,
            "DATE" => return Self::Date,
            "JSON" | "JSONB" => return Self::Json,
            "UUID" | "GUID" => return Self::Uuid,
            _ => {}
        }

        if t.contains("INT") {
            return Self::IntegerAffinity;
        }
        if t.contains("CHAR") || t.contains("CLOB") || t.contains("TEXT") {
            return Self::TextAffinity;
        }
        if t.contains("BLOB") || t.is_empty() {
            return Self::BlobAffinity;
        }
        if t.contains("REAL") || t.contains("FLOA") || t.contains("DOUB") {
            return Self::RealAffinity;
        }
        Self::NumericAffinity
    }

    /// Arrow type to use in the output Parquet schema.
    ///
    /// Note on seconds: Parquet's `TIMESTAMP` logical type only supports
    /// MILLIS / MICROS / NANOS — there is no TIMESTAMP_SECONDS. If we emit
    /// `Timestamp(Second, None)` arrow-rs writes a bare `INT64` with no
    /// logical annotation, so consumers like DuckDB see a plain bigint
    /// instead of a timestamp. We therefore promote `TimestampSecond` to
    /// `Timestamp(Millisecond, None)` and scale numeric seconds-domain
    /// inputs ×1000 in `build_column`. The `TimestampSecond` variant name
    /// still accurately describes the SQLite-side input scale.
    pub fn to_arrow(self) -> DataType {
        match self {
            Self::Boolean => DataType::Boolean,
            Self::TimestampSecond | Self::TimestampMilli => {
                DataType::Timestamp(TimeUnit::Millisecond, None)
            }
            Self::TimestampMicro => DataType::Timestamp(TimeUnit::Microsecond, None),
            Self::TimestampNano => DataType::Timestamp(TimeUnit::Nanosecond, None),
            Self::Date => DataType::Date32,
            Self::Json | Self::Uuid | Self::TextAffinity => DataType::Utf8,
            Self::IntegerAffinity => DataType::Int64,
            Self::BlobAffinity => DataType::Binary,
            // NUMERIC affinity defaults to Float64. Lossless for integers up
            // to 2^53 and for ordinary decimals. For arbitrary precision,
            // declare the column as BIGINT (→ Int64) or wait for a future
            // Decimal128 case here.
            Self::RealAffinity | Self::NumericAffinity => DataType::Float64,
        }
    }

    /// How many of this variant's input units equal one second. Used to
    /// rescale numeric SQLite values (e.g. Unix seconds vs. Unix ms) to
    /// whatever unit `to_arrow()` chose for the Parquet column.
    ///
    /// Returns 0 for non-temporal variants (they never hit the timestamp
    /// builder, but 0 keeps the `i64` type uniform).
    fn input_units_per_second(self) -> i64 {
        match self {
            Self::TimestampSecond => 1,
            Self::TimestampMilli => 1_000,
            Self::TimestampMicro => 1_000_000,
            Self::TimestampNano => 1_000_000_000,
            _ => 0,
        }
    }
}

/// Build a Parquet file directly from SQLite query results. The caller
/// supplies:
///
/// - `pragma_rows`: rows returned by `PRAGMA table_info(tablename)`, i.e. an
///   array of `{ name, type, notnull, pk, dflt_value, cid }` objects. Schema
///   is taken from here; only `name` and `type` are used.
/// - `data_rows`: rows returned by `SELECT * FROM tablename`, i.e. an array
///   of `{ col_name: value, ... }` objects.
///
/// One row group per call, ZSTD compression.
#[wasm_bindgen]
pub fn write_parquet_from_sqlite(
    pragma_rows: JsValue,
    data_rows: JsValue,
) -> Result<Vec<u8>, JsError> {
    let columns: Vec<PragmaColumn> = serde_wasm_bindgen::from_value(pragma_rows)
        .map_err(|e| JsError::new(&format!("pragma parse: {e}")))?;
    if columns.is_empty() {
        return Err(JsError::new("pragma_rows is empty — no schema"));
    }

    let rows = js_sys::Array::from(&data_rows);
    let n = rows.length() as usize;

    let mut fields: Vec<Field> = Vec::with_capacity(columns.len());
    let mut arrays: Vec<ArrayRef> = Vec::with_capacity(columns.len());

    for col in &columns {
        let declared = SqliteDeclared::parse(&col.ty);
        let dt = declared.to_arrow();
        fields.push(Field::new(&col.name, dt.clone(), true));
        let array = build_column(&col.name, declared, &dt, &rows, n)?;
        arrays.push(array);
    }

    let schema = Arc::new(Schema::new(fields));
    let batch = RecordBatch::try_new(schema.clone(), arrays)
        .map_err(|e| JsError::new(&format!("record batch: {e}")))?;

    write_batches_to_parquet(schema, vec![batch])
}

fn get_cell(rows: &js_sys::Array, i: usize, key: &JsValue, name: &str) -> Result<JsValue, JsError> {
    let row = rows.get(i as u32);
    js_sys::Reflect::get(&row, key).map_err(|_| JsError::new(&format!("missing {name} at row {i}")))
}

fn date32_from_str(s: &str) -> Option<i32> {
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1)?;
    Some((d - epoch).num_days() as i32)
}

/// Parse a SQLite datetime string into a `NaiveDateTime` (treated as UTC).
/// Accepts the shapes SQLite drivers actually emit:
///
/// - Naive:  `YYYY-MM-DD HH:MM:SS[.fff]`, `YYYY-MM-DDTHH:MM:SS[.fff]`
/// - Trailing `Z` (UTC)
/// - RFC 3339 with numeric offset: `2026-04-21T12:34:56+02:00`, `...-07:00`
///
/// Offset-qualified values are normalized to UTC so the resulting naive
/// timestamp represents the same instant. Returns `None` for anything we
/// don't recognize — caller writes a null.
fn naive_datetime_from_str(s: &str) -> Option<NaiveDateTime> {
    let trimmed = s.trim();

    // Offset-qualified first: `+HH:MM`, `-HH:MM`, `+HHMM`, `Z`.
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Some(dt.naive_utc());
    }
    if let Ok(dt) = DateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S%.f%:z") {
        return Some(dt.naive_utc());
    }
    if let Ok(dt) = DateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S%:z") {
        return Some(dt.naive_utc());
    }

    // Naive fallbacks (and `Z`-suffixed values that parse_from_rfc3339 missed
    // due to missing seconds precision, etc.).
    let naive = trimmed.trim_end_matches('Z');
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
    ] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(naive, fmt) {
            return Some(dt);
        }
    }
    None
}

/// Tracks how many cells in one column were coerced to null because their
/// runtime storage class didn't match the column's declared type. SQLite
/// allows this (affinity is non-enforcing); we prefer "null this cell" over
/// "reject the whole partition" so one stray row can't kill an export.
///
/// A single summary is emitted to `console.warn` per column at the end.
#[derive(Default)]
struct MismatchStats {
    nulled: usize,
    first_example: Option<String>,
}

impl MismatchStats {
    fn record(&mut self, column: &str, index: usize, reason: &str) {
        self.nulled += 1;
        if self.first_example.is_none() {
            self.first_example = Some(format!("{column}[{index}]: {reason}"));
        }
    }

    fn flush(&self, column: &str) {
        if self.nulled == 0 {
            return;
        }
        let msg = match &self.first_example {
            Some(example) => format!(
                "sift-wasm: nulled {} cell(s) in column {:?} due to type mismatch (first: {})",
                self.nulled, column, example
            ),
            None => format!(
                "sift-wasm: nulled {} cell(s) in column {:?} due to type mismatch",
                self.nulled, column
            ),
        };
        web_sys_console_warn(&msg);
    }
}

#[cfg(target_arch = "wasm32")]
fn web_sys_console_warn(msg: &str) {
    let Ok(global) = js_sys::global().dyn_into::<js_sys::Object>() else {
        return;
    };
    let Ok(console) = js_sys::Reflect::get(&global, &JsValue::from_str("console")) else {
        return;
    };
    let Ok(warn) = js_sys::Reflect::get(&console, &JsValue::from_str("warn")) else {
        return;
    };
    let Ok(warn_fn) = warn.dyn_into::<js_sys::Function>() else {
        return;
    };
    let _ = warn_fn.call1(&console, &JsValue::from_str(msg));
}

#[cfg(not(target_arch = "wasm32"))]
fn web_sys_console_warn(_msg: &str) {
    // Native tests: silently. The test harness asserts nullability directly.
}

/// Build one column from a JS array of row objects.
///
/// Behavior on type mismatch: the SQLite affinity system is permissive, so
/// a value whose runtime storage class doesn't match the column's declared
/// type is written as **null** rather than failing the whole export. A
/// single `console.warn` summary is emitted per column if any cells were
/// nulled. Structural errors (missing column in a row) are still hard
/// errors — those are programmer bugs, not data.
fn build_column(
    name: &str,
    declared: SqliteDeclared,
    dt: &DataType,
    rows: &js_sys::Array,
    n: usize,
) -> Result<ArrayRef, JsError> {
    let key = JsValue::from_str(name);
    let mut stats = MismatchStats::default();

    let array: ArrayRef = match dt {
        DataType::Utf8 => {
            let mut b = StringBuilder::with_capacity(n, n * 32);
            for i in 0..n {
                let v = get_cell(rows, i, &key, name)?;
                if v.is_null() || v.is_undefined() {
                    b.append_null();
                } else if let Some(s) = v.as_string() {
                    b.append_value(s);
                } else {
                    stats.record(
                        name,
                        i,
                        &format!("expected string, got {:?}", v.js_typeof()),
                    );
                    b.append_null();
                }
            }
            Arc::new(b.finish())
        }
        DataType::Int64 => {
            let mut b = Int64Builder::with_capacity(n);
            for i in 0..n {
                let v = get_cell(rows, i, &key, name)?;
                if v.is_null() || v.is_undefined() {
                    b.append_null();
                } else if let Some(f) = v.as_f64() {
                    b.append_value(f as i64);
                } else if let Ok(big) = v.clone().try_into() {
                    b.append_value(big);
                } else {
                    stats.record(
                        name,
                        i,
                        &format!("expected number/bigint, got {:?}", v.js_typeof()),
                    );
                    b.append_null();
                }
            }
            Arc::new(b.finish())
        }
        DataType::Float64 => {
            let mut b = Float64Builder::with_capacity(n);
            for i in 0..n {
                let v = get_cell(rows, i, &key, name)?;
                if v.is_null() || v.is_undefined() {
                    b.append_null();
                } else if let Some(f) = v.as_f64() {
                    b.append_value(f);
                } else {
                    stats.record(
                        name,
                        i,
                        &format!("expected number, got {:?}", v.js_typeof()),
                    );
                    b.append_null();
                }
            }
            Arc::new(b.finish())
        }
        DataType::Boolean => {
            let mut b = BooleanBuilder::with_capacity(n);
            for i in 0..n {
                let v = get_cell(rows, i, &key, name)?;
                if v.is_null() || v.is_undefined() {
                    b.append_null();
                } else if let Some(bl) = v.as_bool() {
                    b.append_value(bl);
                } else if let Some(f) = v.as_f64() {
                    b.append_value(f != 0.0);
                } else {
                    stats.record(
                        name,
                        i,
                        &format!("expected bool/number, got {:?}", v.js_typeof()),
                    );
                    b.append_null();
                }
            }
            Arc::new(b.finish())
        }
        DataType::Timestamp(TimeUnit::Millisecond, _) => {
            let mut b = TimestampMillisecondBuilder::with_capacity(n);
            let col = TimestampColumn::new(name, declared.input_units_per_second(), 1_000);
            for i in 0..n {
                append_timestamp_cell(&mut b, &col, rows, i, &mut stats)?;
            }
            Arc::new(b.finish())
        }
        DataType::Timestamp(TimeUnit::Microsecond, _) => {
            let mut b = TimestampMicrosecondBuilder::with_capacity(n);
            let col = TimestampColumn::new(name, declared.input_units_per_second(), 1_000_000);
            for i in 0..n {
                append_timestamp_cell(&mut b, &col, rows, i, &mut stats)?;
            }
            Arc::new(b.finish())
        }
        DataType::Timestamp(TimeUnit::Nanosecond, _) => {
            let mut b = TimestampNanosecondBuilder::with_capacity(n);
            let col = TimestampColumn::new(name, declared.input_units_per_second(), 1_000_000_000);
            for i in 0..n {
                append_timestamp_cell(&mut b, &col, rows, i, &mut stats)?;
            }
            Arc::new(b.finish())
        }
        DataType::Date32 => {
            let mut b = Date32Builder::with_capacity(n);
            for i in 0..n {
                let v = get_cell(rows, i, &key, name)?;
                if v.is_null() || v.is_undefined() {
                    b.append_null();
                } else if let Some(s) = v.as_string() {
                    match date32_from_str(&s) {
                        Some(d) => b.append_value(d),
                        None => {
                            stats.record(name, i, &format!("unparseable date string {s:?}"));
                            b.append_null();
                        }
                    }
                } else if let Some(f) = v.as_f64() {
                    b.append_value(f as i32);
                } else {
                    stats.record(
                        name,
                        i,
                        "expected 'YYYY-MM-DD' string or days-since-epoch number",
                    );
                    b.append_null();
                }
            }
            Arc::new(b.finish())
        }
        DataType::Binary => {
            let mut b = BinaryBuilder::with_capacity(n, n * 64);
            for i in 0..n {
                let v = get_cell(rows, i, &key, name)?;
                if v.is_null() || v.is_undefined() {
                    b.append_null();
                } else if let Ok(arr_buf) = v.clone().dyn_into::<js_sys::ArrayBuffer>() {
                    let u8 = js_sys::Uint8Array::new(&arr_buf);
                    b.append_value(u8.to_vec().as_slice());
                } else if let Ok(u8) = v.clone().dyn_into::<js_sys::Uint8Array>() {
                    b.append_value(u8.to_vec().as_slice());
                } else {
                    stats.record(name, i, "expected ArrayBuffer or Uint8Array for BLOB");
                    b.append_null();
                }
            }
            Arc::new(b.finish())
        }
        other => {
            return Err(JsError::new(&format!(
                "unsupported Arrow type {other:?} for column {name}"
            )));
        }
    };

    stats.flush(name);
    Ok(array)
}

/// Per-column state for building a timestamp column. Bundled so the
/// per-cell function stays within clippy's argument count.
struct TimestampColumn<'a> {
    /// Column name (used for error messages + mismatch logs).
    name: &'a str,
    /// JS string key used to pluck the value out of each row object.
    key: JsValue,
    /// How many SQLite-side numeric input units equal one second.
    /// 1 for `TIMESTAMP`, 1000 for `DATETIME`/`TIMESTAMP_MS`, etc. Comes
    /// from `SqliteDeclared::input_units_per_second`.
    input_per_second: i64,
    /// How many of the Arrow builder's units equal one second. 1_000 for
    /// Millisecond, 1_000_000 for Microsecond, 1_000_000_000 for Nanosecond.
    output_per_second: i64,
}

impl<'a> TimestampColumn<'a> {
    fn new(name: &'a str, input_per_second: i64, output_per_second: i64) -> Self {
        Self {
            name,
            key: JsValue::from_str(name),
            input_per_second,
            output_per_second,
        }
    }

    /// Scale numeric input → output. `output_per_second ≥ input_per_second`
    /// in every combination we produce, so this is an integer multiply.
    fn numeric_scale(&self) -> i64 {
        if self.input_per_second == 0 {
            1
        } else {
            self.output_per_second / self.input_per_second
        }
    }
}

/// Convert a cell value to an Arrow timestamp at the output unit.
///
/// Numeric inputs are scaled by `output_per_second / input_per_second`
/// (integer division is safe — the builder unit is always ≥ the input
/// unit). String inputs go through `naive_datetime_from_str` to get
/// (seconds, subsec_nanos) and scale into output units.
fn append_timestamp_cell<B>(
    builder: &mut B,
    col: &TimestampColumn<'_>,
    rows: &js_sys::Array,
    i: usize,
    stats: &mut MismatchStats,
) -> Result<(), JsError>
where
    B: TimestampAppender,
{
    let scale_num = col.numeric_scale();
    let v = get_cell(rows, i, &col.key, col.name)?;
    if v.is_null() || v.is_undefined() {
        builder.append_null();
    } else if let Some(f) = v.as_f64() {
        builder.append_value((f as i64) * scale_num);
    } else if let Ok(big) = v.clone().try_into() {
        let big: i64 = big;
        builder.append_value(big * scale_num);
    } else if let Some(s) = v.as_string() {
        match naive_datetime_from_str(&s) {
            Some(dt) => {
                let seconds = dt.and_utc().timestamp();
                let nanos = i64::from(dt.and_utc().timestamp_subsec_nanos());
                let value = match col.output_per_second {
                    1_000 => seconds * 1_000 + nanos / 1_000_000,
                    1_000_000 => seconds * 1_000_000 + nanos / 1_000,
                    1_000_000_000 => seconds * 1_000_000_000 + nanos,
                    _ => seconds * col.output_per_second,
                };
                builder.append_value(value);
            }
            None => {
                stats.record(col.name, i, &format!("unparseable timestamp string {s:?}"));
                builder.append_null();
            }
        }
    } else {
        stats.record(
            col.name,
            i,
            &format!(
                "expected number/bigint/ISO-8601 string, got {:?}",
                v.js_typeof()
            ),
        );
        builder.append_null();
    }
    Ok(())
}

/// Uniform interface for the three timestamp builders so
/// `append_timestamp_cell` doesn't have to be duplicated per unit. There is
/// intentionally no impl for `TimestampSecondBuilder` — Parquet has no
/// TIMESTAMP_SECONDS logical type, so we promote seconds inputs to
/// milliseconds in `SqliteDeclared::to_arrow`.
trait TimestampAppender {
    fn append_value(&mut self, v: i64);
    fn append_null(&mut self);
}

impl TimestampAppender for TimestampMillisecondBuilder {
    fn append_value(&mut self, v: i64) {
        self.append_value(v);
    }
    fn append_null(&mut self) {
        self.append_null();
    }
}

impl TimestampAppender for TimestampMicrosecondBuilder {
    fn append_value(&mut self, v: i64) {
        self.append_value(v);
    }
    fn append_null(&mut self) {
        self.append_null();
    }
}

impl TimestampAppender for TimestampNanosecondBuilder {
    fn append_value(&mut self, v: i64) {
        self.append_value(v);
    }
    fn append_null(&mut self) {
        self.append_null();
    }
}

fn write_batches_to_parquet(
    schema: Arc<Schema>,
    batches: Vec<RecordBatch>,
) -> Result<Vec<u8>, JsError> {
    let zstd_level =
        ZstdLevel::try_new(3).map_err(|e| JsError::new(&format!("zstd level: {e}")))?;
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(zstd_level))
        .build();

    let mut buf: Vec<u8> = Vec::new();
    let mut writer = ArrowWriter::try_new(&mut buf, schema, Some(props))
        .map_err(|e| JsError::new(&format!("parquet writer: {e}")))?;
    for batch in batches {
        writer
            .write(&batch)
            .map_err(|e| JsError::new(&format!("parquet write: {e}")))?;
    }
    writer
        .close()
        .map_err(|e| JsError::new(&format!("parquet close: {e}")))?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_explicit_richer_types() {
        use SqliteDeclared::*;
        assert_eq!(SqliteDeclared::parse("BOOLEAN"), Boolean);
        assert_eq!(SqliteDeclared::parse("bool"), Boolean);
        assert_eq!(SqliteDeclared::parse("BIT"), Boolean);
        assert_eq!(SqliteDeclared::parse("TIMESTAMP"), TimestampSecond);
        assert_eq!(SqliteDeclared::parse("timestamptz"), TimestampSecond);
        assert_eq!(SqliteDeclared::parse("TIMESTAMP_S"), TimestampSecond);
        assert_eq!(SqliteDeclared::parse("DATETIME"), TimestampMilli);
        assert_eq!(SqliteDeclared::parse("TIMESTAMP_MS"), TimestampMilli);
        assert_eq!(SqliteDeclared::parse("TIMESTAMP_US"), TimestampMicro);
        assert_eq!(SqliteDeclared::parse("TIMESTAMP_NS"), TimestampNano);
        assert_eq!(SqliteDeclared::parse("DATE"), Date);
        assert_eq!(SqliteDeclared::parse("JSON"), Json);
        assert_eq!(SqliteDeclared::parse("jsonb"), Json);
        assert_eq!(SqliteDeclared::parse("uuid"), Uuid);
        assert_eq!(SqliteDeclared::parse("GUID"), Uuid);
    }

    #[test]
    fn parses_sqlite_affinity_by_substring() {
        use SqliteDeclared::*;
        assert_eq!(SqliteDeclared::parse("INTEGER"), IntegerAffinity);
        assert_eq!(SqliteDeclared::parse("BIGINT"), IntegerAffinity);
        assert_eq!(SqliteDeclared::parse("SMALLINT"), IntegerAffinity);
        assert_eq!(SqliteDeclared::parse("TINYINT"), IntegerAffinity);
        assert_eq!(SqliteDeclared::parse("MEDIUMINT"), IntegerAffinity);
        assert_eq!(SqliteDeclared::parse("INT2"), IntegerAffinity);
        assert_eq!(SqliteDeclared::parse("INT8"), IntegerAffinity);
        assert_eq!(SqliteDeclared::parse("TEXT"), TextAffinity);
        assert_eq!(SqliteDeclared::parse("VARCHAR(100)"), TextAffinity);
        assert_eq!(SqliteDeclared::parse("NVARCHAR(50)"), TextAffinity);
        assert_eq!(SqliteDeclared::parse("NCHAR"), TextAffinity);
        assert_eq!(SqliteDeclared::parse("CHARACTER(20)"), TextAffinity);
        assert_eq!(SqliteDeclared::parse("CLOB"), TextAffinity);
        assert_eq!(SqliteDeclared::parse("BLOB"), BlobAffinity);
        assert_eq!(SqliteDeclared::parse(""), BlobAffinity);
        assert_eq!(SqliteDeclared::parse("REAL"), RealAffinity);
        assert_eq!(SqliteDeclared::parse("DOUBLE PRECISION"), RealAffinity);
        assert_eq!(SqliteDeclared::parse("FLOAT"), RealAffinity);
        assert_eq!(SqliteDeclared::parse("NUMERIC"), NumericAffinity);
        assert_eq!(SqliteDeclared::parse("DECIMAL(10,2)"), NumericAffinity);
        assert_eq!(SqliteDeclared::parse("NUMBER"), NumericAffinity);
    }

    #[test]
    fn maps_to_expected_arrow_types() {
        assert_eq!(SqliteDeclared::Boolean.to_arrow(), DataType::Boolean);
        // TimestampSecond promotes to Millisecond in Parquet (no
        // TIMESTAMP_SECONDS logical type exists).
        assert_eq!(
            SqliteDeclared::TimestampSecond.to_arrow(),
            DataType::Timestamp(TimeUnit::Millisecond, None)
        );
        assert_eq!(
            SqliteDeclared::TimestampMilli.to_arrow(),
            DataType::Timestamp(TimeUnit::Millisecond, None)
        );
        assert_eq!(
            SqliteDeclared::TimestampMicro.to_arrow(),
            DataType::Timestamp(TimeUnit::Microsecond, None)
        );
        assert_eq!(
            SqliteDeclared::TimestampNano.to_arrow(),
            DataType::Timestamp(TimeUnit::Nanosecond, None)
        );
        assert_eq!(SqliteDeclared::Date.to_arrow(), DataType::Date32);
        assert_eq!(SqliteDeclared::Json.to_arrow(), DataType::Utf8);
        assert_eq!(SqliteDeclared::Uuid.to_arrow(), DataType::Utf8);
        assert_eq!(SqliteDeclared::IntegerAffinity.to_arrow(), DataType::Int64);
        assert_eq!(SqliteDeclared::TextAffinity.to_arrow(), DataType::Utf8);
        assert_eq!(SqliteDeclared::BlobAffinity.to_arrow(), DataType::Binary);
        assert_eq!(SqliteDeclared::RealAffinity.to_arrow(), DataType::Float64);
        assert_eq!(
            SqliteDeclared::NumericAffinity.to_arrow(),
            DataType::Float64
        );
    }
}
