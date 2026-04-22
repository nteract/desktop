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
    TimestampSecondBuilder,
};
use arrow::array::{ArrayRef, RecordBatch};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::ipc::reader::StreamReader;
use chrono::NaiveDate;
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
    pub fn to_arrow(self) -> DataType {
        match self {
            Self::Boolean => DataType::Boolean,
            Self::TimestampSecond => DataType::Timestamp(TimeUnit::Second, None),
            Self::TimestampMilli => DataType::Timestamp(TimeUnit::Millisecond, None),
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
}

fn sqlite_type_to_arrow(declared: &str) -> DataType {
    SqliteDeclared::parse(declared).to_arrow()
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
        let dt = sqlite_type_to_arrow(&col.ty);
        fields.push(Field::new(&col.name, dt.clone(), true));
        let array = build_column(&col.name, &dt, &rows, n)?;
        arrays.push(array);
    }

    let schema = Arc::new(Schema::new(fields));
    let batch = RecordBatch::try_new(schema.clone(), arrays)
        .map_err(|e| JsError::new(&format!("record batch: {e}")))?;

    write_batches_to_parquet(schema, vec![batch])
}

fn get_cell(
    rows: &js_sys::Array,
    i: usize,
    key: &JsValue,
    name: &str,
) -> Result<JsValue, JsError> {
    let row = rows.get(i as u32);
    js_sys::Reflect::get(&row, key)
        .map_err(|_| JsError::new(&format!("missing {name} at row {i}")))
}

fn date32_from_str(s: &str) -> Result<i32, JsError> {
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|e| JsError::new(&format!("date parse '{s}': {e}")))?;
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
    Ok((d - epoch).num_days() as i32)
}

fn build_column(
    name: &str,
    dt: &DataType,
    rows: &js_sys::Array,
    n: usize,
) -> Result<ArrayRef, JsError> {
    let key = JsValue::from_str(name);
    match dt {
        DataType::Utf8 => {
            let mut b = StringBuilder::with_capacity(n, n * 32);
            for i in 0..n {
                let v = get_cell(rows, i, &key, name)?;
                if v.is_null() || v.is_undefined() {
                    b.append_null();
                } else if let Some(s) = v.as_string() {
                    b.append_value(s);
                } else {
                    return Err(JsError::new(&format!(
                        "{name}[{i}]: expected string, got {:?}",
                        v.js_typeof()
                    )));
                }
            }
            Ok(Arc::new(b.finish()))
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
                    return Err(JsError::new(&format!(
                        "{name}[{i}]: expected number/bigint, got {:?}",
                        v.js_typeof()
                    )));
                }
            }
            Ok(Arc::new(b.finish()))
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
                    return Err(JsError::new(&format!(
                        "{name}[{i}]: expected number, got {:?}",
                        v.js_typeof()
                    )));
                }
            }
            Ok(Arc::new(b.finish()))
        }
        DataType::Boolean => {
            let mut b = BooleanBuilder::with_capacity(n);
            for i in 0..n {
                let v = get_cell(rows, i, &key, name)?;
                if v.is_null() || v.is_undefined() {
                    b.append_null();
                } else if let Some(f) = v.as_f64() {
                    b.append_value(f != 0.0);
                } else if let Some(bl) = v.as_bool() {
                    b.append_value(bl);
                } else {
                    return Err(JsError::new(&format!(
                        "{name}[{i}]: expected number/bool (SQLite bool), got {:?}",
                        v.js_typeof()
                    )));
                }
            }
            Ok(Arc::new(b.finish()))
        }
        DataType::Timestamp(TimeUnit::Second, _) => {
            let mut b = TimestampSecondBuilder::with_capacity(n);
            for i in 0..n {
                let v = get_cell(rows, i, &key, name)?;
                if v.is_null() || v.is_undefined() {
                    b.append_null();
                } else if let Some(f) = v.as_f64() {
                    b.append_value(f as i64);
                } else if let Ok(big) = v.clone().try_into() {
                    b.append_value(big);
                } else {
                    return Err(JsError::new(&format!(
                        "{name}[{i}]: expected number/bigint unix seconds"
                    )));
                }
            }
            Ok(Arc::new(b.finish()))
        }
        DataType::Timestamp(TimeUnit::Millisecond, _) => {
            let mut b = TimestampMillisecondBuilder::with_capacity(n);
            for i in 0..n {
                let v = get_cell(rows, i, &key, name)?;
                if v.is_null() || v.is_undefined() {
                    b.append_null();
                } else if let Some(f) = v.as_f64() {
                    b.append_value(f as i64);
                } else if let Ok(big) = v.clone().try_into() {
                    b.append_value(big);
                } else {
                    return Err(JsError::new(&format!(
                        "{name}[{i}]: expected number/bigint unix milliseconds"
                    )));
                }
            }
            Ok(Arc::new(b.finish()))
        }
        DataType::Timestamp(TimeUnit::Microsecond, _) => {
            let mut b = TimestampMicrosecondBuilder::with_capacity(n);
            for i in 0..n {
                let v = get_cell(rows, i, &key, name)?;
                if v.is_null() || v.is_undefined() {
                    b.append_null();
                } else if let Some(f) = v.as_f64() {
                    b.append_value(f as i64);
                } else if let Ok(big) = v.clone().try_into() {
                    b.append_value(big);
                } else {
                    return Err(JsError::new(&format!(
                        "{name}[{i}]: expected number/bigint unix microseconds"
                    )));
                }
            }
            Ok(Arc::new(b.finish()))
        }
        DataType::Timestamp(TimeUnit::Nanosecond, _) => {
            let mut b = TimestampNanosecondBuilder::with_capacity(n);
            for i in 0..n {
                let v = get_cell(rows, i, &key, name)?;
                if v.is_null() || v.is_undefined() {
                    b.append_null();
                } else if let Some(f) = v.as_f64() {
                    b.append_value(f as i64);
                } else if let Ok(big) = v.clone().try_into() {
                    b.append_value(big);
                } else {
                    return Err(JsError::new(&format!(
                        "{name}[{i}]: expected number/bigint unix nanoseconds"
                    )));
                }
            }
            Ok(Arc::new(b.finish()))
        }
        DataType::Date32 => {
            let mut b = Date32Builder::with_capacity(n);
            for i in 0..n {
                let v = get_cell(rows, i, &key, name)?;
                if v.is_null() || v.is_undefined() {
                    b.append_null();
                } else if let Some(s) = v.as_string() {
                    b.append_value(date32_from_str(&s)?);
                } else if let Some(f) = v.as_f64() {
                    b.append_value(f as i32);
                } else {
                    return Err(JsError::new(&format!(
                        "{name}[{i}]: expected 'YYYY-MM-DD' string or days-since-epoch number"
                    )));
                }
            }
            Ok(Arc::new(b.finish()))
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
                    return Err(JsError::new(&format!(
                        "{name}[{i}]: expected ArrayBuffer or Uint8Array for BLOB"
                    )));
                }
            }
            Ok(Arc::new(b.finish()))
        }
        other => Err(JsError::new(&format!(
            "unsupported Arrow type {other:?} for column {name}"
        ))),
    }
}

fn write_batches_to_parquet(
    schema: Arc<Schema>,
    batches: Vec<RecordBatch>,
) -> Result<Vec<u8>, JsError> {
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(3).unwrap()))
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
        assert_eq!(
            SqliteDeclared::TimestampSecond.to_arrow(),
            DataType::Timestamp(TimeUnit::Second, None)
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
        assert_eq!(SqliteDeclared::NumericAffinity.to_arrow(), DataType::Float64);
    }
}
