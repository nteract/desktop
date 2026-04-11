use arrow::array::{Array, AsArray, BooleanArray, StringArray};
use arrow::datatypes::DataType;
use arrow::ipc::reader::StreamReader;
use arrow::ipc::writer::StreamWriter;
use arrow_select::filter::filter_record_batch;
use std::io::Cursor;

use crate::utils::dict_key_at;

/// Filter rows by a boolean mask, return filtered Arrow IPC bytes.
pub fn filter_rows_impl(
    ipc_bytes: &[u8],
    mask: &[u8],
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let cursor = Cursor::new(ipc_bytes);
    let reader = StreamReader::try_new(cursor, None)?;
    let schema = reader.schema();

    let mut output = Vec::new();
    let mut writer = StreamWriter::try_new(&mut output, &schema)?;

    let mut offset = 0;
    for batch in reader {
        let batch = batch?;
        let batch_len = batch.num_rows();

        // Build boolean mask for this batch
        let batch_mask: Vec<bool> = (0..batch_len)
            .map(|i| {
                let global_idx = offset + i;
                global_idx < mask.len() && mask[global_idx] != 0
            })
            .collect();

        let bool_arr = BooleanArray::from(batch_mask);
        let filtered = filter_record_batch(&batch, &bool_arr)?;

        if filtered.num_rows() > 0 {
            writer.write(&filtered)?;
        }

        offset += batch_len;
    }

    writer.finish()?;
    drop(writer);
    Ok(output)
}

/// Find row indices where a string column contains the given substring.
pub fn string_contains_impl(
    ipc_bytes: &[u8],
    column_index: usize,
    query: &str,
) -> Result<Vec<u32>, Box<dyn std::error::Error>> {
    let cursor = Cursor::new(ipc_bytes);
    let reader = StreamReader::try_new(cursor, None)?;

    let query_lower = query.to_lowercase();
    let mut indices = Vec::new();
    let mut offset: u32 = 0;

    for batch in reader {
        let batch = batch?;
        let col = batch.column(column_index);

        match col.data_type() {
            DataType::Utf8 | DataType::LargeUtf8 => {
                let arr = col
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .ok_or("expected StringArray for Utf8 column")?;
                for i in 0..arr.len() {
                    if !arr.is_null(i) && arr.value(i).to_lowercase().contains(&query_lower) {
                        indices.push(offset + i as u32);
                    }
                }
            }
            DataType::Dictionary(_, _) => {
                let dict_arr = col.as_any_dictionary();
                let keys = dict_arr.keys();
                let values = dict_arr.values();
                let str_values = values
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .ok_or("expected StringArray for dictionary values")?;

                // Pre-check which dictionary values match (much faster for repeated values)
                let dict_matches: Vec<bool> = (0..str_values.len())
                    .map(|i| {
                        if str_values.is_null(i) {
                            false
                        } else {
                            str_values.value(i).to_lowercase().contains(&query_lower)
                        }
                    })
                    .collect();

                for i in 0..keys.len() {
                    if let Some(key) = dict_key_at(keys, i) {
                        if key < dict_matches.len() && dict_matches[key] {
                            indices.push(offset + i as u32);
                        }
                    }
                }
            }
            _ => {}
        }

        offset += batch.num_rows() as u32;
    }

    Ok(indices)
}
