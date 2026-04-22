//! SQLite → Parquet round-trip for `write_parquet_from_sqlite`.
//!
//! Needs a real wasm-bindgen JsValue for the PRAGMA + data-row fixtures, so
//! it runs in the wasm32 target via `wasm-pack test --node crates/sift-wasm`.

#![cfg(target_arch = "wasm32")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use arrow::array::{
    Array, BooleanArray, Date32Array, Float64Array, Int64Array, RecordBatch, StringArray,
    TimestampMillisecondArray,
};
use arrow::datatypes::{DataType, TimeUnit};
use bytes::Bytes;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::basic::LogicalType;
use serde::Serialize;
use sift_wasm::write_parquet_from_sqlite;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_node_experimental);

/// Serialize to JsValue with `serialize_maps_as_objects(true)` so serde maps
/// become plain JS objects (the shape real SQLite drivers return), not
/// `Map` instances.
fn to_js(v: &serde_json::Value) -> JsValue {
    let serializer = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
    v.serialize(&serializer).expect("serialize json to JsValue")
}

#[wasm_bindgen_test]
fn sqlite_round_trip_preserves_richer_types() {
    let pragma = to_js(&serde_json::json!([
        {"cid": 0, "name": "id",          "type": "INTEGER",   "notnull": 0, "dflt_value": null, "pk": 1},
        {"cid": 1, "name": "label",       "type": "TEXT",      "notnull": 0, "dflt_value": null, "pk": 0},
        {"cid": 2, "name": "ratio",       "type": "REAL",      "notnull": 0, "dflt_value": null, "pk": 0},
        {"cid": 3, "name": "active",      "type": "BOOLEAN",   "notnull": 0, "dflt_value": null, "pk": 0},
        {"cid": 4, "name": "received_at", "type": "TIMESTAMP", "notnull": 0, "dflt_value": null, "pk": 0},
        {"cid": 5, "name": "day",         "type": "DATE",      "notnull": 0, "dflt_value": null, "pk": 0},
    ]));

    let rows = to_js(&serde_json::json!([
        {
            // received_at is Unix seconds (SQLite TIMESTAMP convention).
            // We scale ×1000 so Parquet can carry it as a TIMESTAMP_MILLIS
            // logical type (Parquet has no TIMESTAMP_SECONDS).
            "id": 1, "label": "alpha", "ratio": 0.5,
            "active": 1, "received_at": 1_776_831_852_i64,
            "day": "2026-04-20"
        },
        {
            "id": 2, "label": "beta", "ratio": 1.25,
            "active": 0, "received_at": 1_776_918_252_i64,
            "day": "2026-04-21"
        },
    ]));

    let parquet = write_parquet_from_sqlite(pragma, rows).expect("write_parquet_from_sqlite");

    assert!(parquet.starts_with(b"PAR1"), "missing leading magic");
    assert!(parquet.ends_with(b"PAR1"), "missing trailing magic");

    let builder = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(parquet))
        .expect("parquet reader builder");
    let schema = Arc::clone(builder.schema());

    // Assert the Parquet *physical* schema carries a TIMESTAMP logical type
    // annotation. The Arrow schema we read above could otherwise report a
    // timestamp even when the physical schema has no annotation (arrow-rs
    // embeds an `ARROW:schema` metadata blob), so downstream consumers like
    // DuckDB / Polars / pyarrow's non-Arrow-aware paths wouldn't recognize
    // it. This is the invariant that catches the seconds→int64 regression.
    let parquet_meta = builder.metadata();
    let received_descr = parquet_meta.file_metadata().schema_descr().column(4);
    assert!(
        matches!(
            received_descr.logical_type(),
            Some(LogicalType::Timestamp { .. })
        ),
        "received_at must carry a Parquet TIMESTAMP logical type, got {:?}",
        received_descr.logical_type()
    );

    // Schema must carry the richer types — not VARCHAR-for-everything.
    assert_eq!(schema.field(0).name(), "id");
    assert_eq!(schema.field(0).data_type(), &DataType::Int64);
    assert_eq!(schema.field(1).name(), "label");
    assert_eq!(schema.field(1).data_type(), &DataType::Utf8);
    assert_eq!(schema.field(2).name(), "ratio");
    assert_eq!(schema.field(2).data_type(), &DataType::Float64);
    assert_eq!(schema.field(3).name(), "active");
    assert_eq!(schema.field(3).data_type(), &DataType::Boolean);
    assert_eq!(schema.field(4).name(), "received_at");
    assert_eq!(
        schema.field(4).data_type(),
        &DataType::Timestamp(TimeUnit::Millisecond, None)
    );
    assert_eq!(schema.field(5).name(), "day");
    assert_eq!(schema.field(5).data_type(), &DataType::Date32);

    let reader = builder.build().expect("build reader");
    let batches: Vec<RecordBatch> = reader.collect::<Result<_, _>>().expect("read batches");
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 2);

    let batch = batches.into_iter().next().expect("one batch");

    let ids = batch
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("int64");
    assert_eq!(ids.value(0), 1);
    assert_eq!(ids.value(1), 2);

    let labels = batch
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("utf8");
    assert_eq!(labels.value(0), "alpha");
    assert_eq!(labels.value(1), "beta");

    let ratio = batch
        .column(2)
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("float64");
    assert!((ratio.value(0) - 0.5).abs() < 1e-12);
    assert!((ratio.value(1) - 1.25).abs() < 1e-12);

    let active = batch
        .column(3)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .expect("boolean");
    assert!(active.value(0));
    assert!(!active.value(1));

    // Seconds input (1_776_831_852) scales ×1000 into Millisecond builder.
    let received = batch
        .column(4)
        .as_any()
        .downcast_ref::<TimestampMillisecondArray>()
        .expect("timestamp(ms)");
    assert_eq!(received.value(0), 1_776_831_852_000);
    assert_eq!(received.value(1), 1_776_918_252_000);

    let day = batch
        .column(5)
        .as_any()
        .downcast_ref::<Date32Array>()
        .expect("date32");
    // 2026-04-20 = days since epoch
    let epoch_days_20 = (chrono::NaiveDate::from_ymd_opt(2026, 4, 20).unwrap()
        - chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap())
    .num_days() as i32;
    let epoch_days_21 = (chrono::NaiveDate::from_ymd_opt(2026, 4, 21).unwrap()
        - chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap())
    .num_days() as i32;
    assert_eq!(day.value(0), epoch_days_20);
    assert_eq!(day.value(1), epoch_days_21);
}

#[wasm_bindgen_test]
fn sqlite_nulls_round_trip() {
    let pragma = to_js(&serde_json::json!([
        {"name": "id",    "type": "INTEGER"},
        {"name": "label", "type": "TEXT"},
    ]));
    let rows = to_js(&serde_json::json!([
        {"id": 1, "label": "alpha"},
        {"id": null, "label": null},
    ]));

    let parquet = write_parquet_from_sqlite(pragma, rows).expect("write_parquet_from_sqlite");
    assert!(parquet.starts_with(b"PAR1"));
    assert!(parquet.ends_with(b"PAR1"));

    let reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(parquet))
        .expect("parquet reader")
        .build()
        .expect("build reader");

    let batch = reader
        .into_iter()
        .next()
        .expect("one batch")
        .expect("batch ok");

    assert_eq!(batch.num_rows(), 2);

    let ids = batch
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("int64");
    assert!(!ids.is_null(0));
    assert_eq!(ids.value(0), 1);
    assert!(ids.is_null(1));

    let labels = batch
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("utf8");
    assert!(!labels.is_null(0));
    assert_eq!(labels.value(0), "alpha");
    assert!(labels.is_null(1));
}

/// P1: text-encoded SQLite timestamps (the common case — SQLite stores
/// TIMESTAMP/DATETIME as ISO-8601 text in practice) round-trip instead of
/// rejecting the export. Covers naive forms, trailing `Z`, and numeric
/// offsets (`TIMESTAMPTZ` with `+HH:MM`/`-HH:MM`).
#[wasm_bindgen_test]
fn sqlite_text_timestamps_round_trip() {
    let pragma = to_js(&serde_json::json!([
        {"name": "ts_s",   "type": "TIMESTAMP"},
        {"name": "ts_ms",  "type": "DATETIME"},
        {"name": "ts_tz",  "type": "TIMESTAMPTZ"},
    ]));
    let rows = to_js(&serde_json::json!([
        {
            "ts_s": "2026-04-21 12:34:56",
            "ts_ms": "2026-04-21T12:34:56.789",
            // 14:34:56+02:00 == 12:34:56Z
            "ts_tz": "2026-04-21T14:34:56+02:00"
        },
        {
            "ts_s": "2026-04-21T12:34:56Z",
            "ts_ms": "2026-04-21 12:34:56",
            // 05:34:56-07:00 == 12:34:56Z
            "ts_tz": "2026-04-21T05:34:56-07:00"
        },
    ]));

    let parquet = write_parquet_from_sqlite(pragma, rows).expect("write_parquet_from_sqlite");

    let reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(parquet))
        .expect("parquet reader")
        .build()
        .expect("build reader");
    let batch = reader
        .into_iter()
        .next()
        .expect("one batch")
        .expect("batch ok");

    assert_eq!(batch.num_rows(), 2);

    // TIMESTAMP promotes to Millisecond in Parquet; string-parsed seconds
    // are scaled to ms. 2026-04-21 12:34:56 UTC → 1_776_774_896_000.
    let ts_s = batch
        .column(0)
        .as_any()
        .downcast_ref::<TimestampMillisecondArray>()
        .expect("timestamp(ms)");
    assert_eq!(ts_s.value(0), 1_776_774_896_000);
    assert_eq!(ts_s.value(1), 1_776_774_896_000);

    let ts_ms = batch
        .column(1)
        .as_any()
        .downcast_ref::<TimestampMillisecondArray>()
        .expect("timestamp(ms)");
    assert_eq!(ts_ms.value(0), 1_776_774_896_789);
    assert_eq!(ts_ms.value(1), 1_776_774_896_000);

    // TIMESTAMPTZ also promotes to Millisecond; both offset-qualified values
    // normalize to the same UTC instant.
    let ts_tz = batch
        .column(2)
        .as_any()
        .downcast_ref::<TimestampMillisecondArray>()
        .expect("timestamptz (ms)");
    assert!(!ts_tz.is_null(0), "positive offset must parse, not null");
    assert!(!ts_tz.is_null(1), "negative offset must parse, not null");
    assert_eq!(ts_tz.value(0), 1_776_774_896_000);
    assert_eq!(ts_tz.value(1), 1_776_774_896_000);
}

/// P2: SQLite rows whose runtime storage class doesn't match the declared
/// affinity produce null for that cell, not a whole-export failure.
/// Codex flagged this: `INTEGER` columns with legacy/permissive data can
/// return text; we null-on-mismatch rather than abort.
#[wasm_bindgen_test]
fn sqlite_mismatch_nulls_the_cell_not_the_export() {
    let pragma = to_js(&serde_json::json!([
        {"name": "id",    "type": "INTEGER"},
        {"name": "ratio", "type": "REAL"},
        {"name": "day",   "type": "DATE"},
        {"name": "ts",    "type": "TIMESTAMP"},
    ]));
    let rows = to_js(&serde_json::json!([
        {"id": 1,     "ratio": 0.5,  "day": "2026-04-20",  "ts": 1_777_000_000_i64},
        {"id": "abc", "ratio": "x",  "day": "not-a-date",  "ts": "also not a timestamp"},
        {"id": 3,     "ratio": 1.25, "day": "2026-04-22",  "ts": "2026-04-22 00:00:00"},
    ]));

    let parquet = write_parquet_from_sqlite(pragma, rows).expect("write_parquet_from_sqlite");
    let reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(parquet))
        .expect("parquet reader")
        .build()
        .expect("build reader");
    let batch = reader
        .into_iter()
        .next()
        .expect("one batch")
        .expect("batch ok");

    assert_eq!(batch.num_rows(), 3);

    let ids = batch
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("int64");
    assert!(!ids.is_null(0));
    assert_eq!(ids.value(0), 1);
    assert!(ids.is_null(1), "text 'abc' in INTEGER column should null");
    assert!(!ids.is_null(2));
    assert_eq!(ids.value(2), 3);

    let ratio = batch
        .column(1)
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("float64");
    assert!(!ratio.is_null(0));
    assert!(ratio.is_null(1), "text 'x' in REAL column should null");
    assert!(!ratio.is_null(2));

    let day = batch
        .column(2)
        .as_any()
        .downcast_ref::<Date32Array>()
        .expect("date32");
    assert!(!day.is_null(0));
    assert!(day.is_null(1), "unparseable date string should null");
    assert!(!day.is_null(2));

    let ts = batch
        .column(3)
        .as_any()
        .downcast_ref::<TimestampMillisecondArray>()
        .expect("timestamp(ms)");
    assert!(!ts.is_null(0));
    assert!(ts.is_null(1), "unparseable timestamp string should null");
    assert!(!ts.is_null(2));
}
