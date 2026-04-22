//! Arrow IPC → Parquet round-trip for `write_parquet_ipc`. Native test.
//!
//! Lives on the native target because it doesn't touch `JsValue`.

#![cfg(not(target_arch = "wasm32"))]

use std::io::Cursor;
use std::sync::Arc;

use arrow::array::{Int64Array, RecordBatch, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::ipc::writer::StreamWriter;
use bytes::Bytes;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use sift_wasm::write_parquet_ipc;

fn sample_ipc() -> (Vec<u8>, Arc<Schema>) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, true),
        Field::new("label", DataType::Utf8, true),
    ]));

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(vec![1i64, 2, 3])),
            Arc::new(StringArray::from(vec![
                Some("alpha"),
                Some("beta"),
                Some("gamma"),
            ])),
        ],
    )
    .expect("record batch");

    let mut buf: Vec<u8> = Vec::new();
    {
        let mut writer = StreamWriter::try_new(&mut buf, &schema).expect("stream writer");
        writer.write(&batch).expect("ipc write");
        writer.finish().expect("ipc finish");
    }
    (buf, schema)
}

#[test]
fn ipc_round_trip_is_valid_parquet() {
    let (ipc, _schema) = sample_ipc();
    let parquet = write_parquet_ipc(&ipc).expect("write_parquet_ipc");

    assert!(parquet.starts_with(b"PAR1"), "missing leading magic");
    assert!(parquet.ends_with(b"PAR1"), "missing trailing magic");

    let reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(parquet))
        .expect("parquet reader")
        .build()
        .expect("build reader");

    let batches: Vec<RecordBatch> = reader.collect::<Result<_, _>>().expect("read batches");
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 3, "expected 3 rows");

    let batch = batches.into_iter().next().expect("one batch");
    let ids = batch
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("int64 column");
    let labels = batch
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("utf8 column");

    assert_eq!(ids.value(0), 1);
    assert_eq!(ids.value(2), 3);
    assert_eq!(labels.value(0), "alpha");
    assert_eq!(labels.value(2), "gamma");
}

#[test]
fn empty_ipc_stream_produces_valid_parquet() {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, true)]));
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut writer = StreamWriter::try_new(&mut buf, &schema).expect("stream writer");
        writer.finish().expect("ipc finish");
    }

    let parquet = write_parquet_ipc(&buf).expect("write_parquet_ipc");
    assert!(parquet.starts_with(b"PAR1"));
    assert!(parquet.ends_with(b"PAR1"));

    // Parse headers through a cursor so malformed output would panic here.
    let _ = Cursor::new(&parquet);
    let reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(parquet))
        .expect("parquet reader")
        .build()
        .expect("build reader");
    let batches: Vec<RecordBatch> = reader.collect::<Result<_, _>>().expect("read batches");
    let total: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total, 0);
}
