//! SQLite → Parquet round-trip for `write_parquet_from_sqlite`.
//!
//! Needs a real wasm-bindgen JsValue for the PRAGMA + data-row fixtures, so
//! it runs in the wasm32 target via `wasm-pack test --node crates/sift-wasm`.

#![cfg(target_arch = "wasm32")]

use std::sync::Arc;

use arrow::array::{
    Array, BooleanArray, Date32Array, Float64Array, Int64Array, RecordBatch, StringArray,
    TimestampSecondArray,
};
use arrow::datatypes::{DataType, TimeUnit};
use bytes::Bytes;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
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
        &DataType::Timestamp(TimeUnit::Second, None)
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

    let received = batch
        .column(4)
        .as_any()
        .downcast_ref::<TimestampSecondArray>()
        .expect("timestamp(s)");
    assert_eq!(received.value(0), 1_776_831_852);
    assert_eq!(received.value(1), 1_776_918_252);

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
