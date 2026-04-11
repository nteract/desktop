use arrow::array::{Array, AsArray, Float64Array, Int32Array, Int64Array, StringArray};
use arrow::datatypes::DataType;
use arrow::ipc::reader::StreamReader;
use arrow_cast::display::ArrayFormatter;
use serde::Serialize;
use std::collections::HashMap;
use std::io::Cursor;
use wasm_bindgen::JsValue;

use crate::utils::dict_key_at;

#[derive(Serialize)]
pub struct CategoryCount {
    pub label: String,
    pub count: u32,
}

#[derive(Serialize)]
pub struct HistogramBin {
    pub x0: f64,
    pub x1: f64,
    pub count: u32,
}

/// Compute value_counts for a string column from Arrow IPC bytes.
pub fn value_counts_impl(
    ipc_bytes: &[u8],
    column_index: usize,
) -> Result<JsValue, Box<dyn std::error::Error>> {
    let cursor = Cursor::new(ipc_bytes);
    let reader = StreamReader::try_new(cursor, None)?;

    let mut freq: HashMap<String, u32> = HashMap::new();

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
                    if !arr.is_null(i) {
                        *freq.entry(arr.value(i).to_string()).or_insert(0) += 1;
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
                for i in 0..keys.len() {
                    if let Some(key) = dict_key_at(keys, i) {
                        let val = str_values.value(key);
                        *freq.entry(val.to_string()).or_insert(0) += 1;
                    }
                }
            }
            _ => {
                // Fallback: format individual values via arrow display
                if let Ok(formatter) = ArrayFormatter::try_new(col.as_ref(), &Default::default()) {
                    for i in 0..col.len() {
                        if !col.is_null(i) {
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

    Ok(serde_wasm_bindgen::to_value(&counts)?)
}

/// Compute histogram bins for a numeric column from Arrow IPC bytes.
pub fn histogram_impl(
    ipc_bytes: &[u8],
    column_index: usize,
    num_bins: usize,
) -> Result<JsValue, Box<dyn std::error::Error>> {
    let cursor = Cursor::new(ipc_bytes);
    let reader = StreamReader::try_new(cursor, None)?;

    let mut values: Vec<f64> = Vec::new();

    for batch in reader {
        let batch = batch?;
        let col = batch.column(column_index);

        match col.data_type() {
            DataType::Float64 => {
                if let Some(arr) = col.as_any().downcast_ref::<Float64Array>() {
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
                if let Some(arr) = col.as_any().downcast_ref::<Int32Array>() {
                    for i in 0..arr.len() {
                        if !arr.is_null(i) {
                            values.push(arr.value(i) as f64);
                        }
                    }
                }
            }
            DataType::Int64 => {
                if let Some(arr) = col.as_any().downcast_ref::<Int64Array>() {
                    for i in 0..arr.len() {
                        if !arr.is_null(i) {
                            values.push(arr.value(i) as f64);
                        }
                    }
                }
            }
            _ => {
                return Err(format!("Unsupported numeric type: {:?}", col.data_type()).into());
            }
        }
    }

    if values.is_empty() {
        return Ok(serde_wasm_bindgen::to_value(&Vec::<HistogramBin>::new())?);
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
        // Floating-point precision correction (à la Polars uniform_hist_count):
        // the division may place a value in an adjacent bin by ±1 ULP.
        if idx > 0 && *v < bins[idx].x0 {
            idx -= 1;
        } else if idx + 1 < num_bins && *v >= bins[idx + 1].x0 {
            idx += 1;
        }
        bins[idx].count += 1;
    }

    Ok(serde_wasm_bindgen::to_value(&bins)?)
}
