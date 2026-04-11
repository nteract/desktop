use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::JsValue;

mod filter;
mod store;
mod summary;
mod utils;

/// Initialize the WASM module. Call once before using other functions.
/// Sets up panic hook so Rust panics show readable messages in the browser console.
#[wasm_bindgen(start)]
pub fn init() {
    console_error_panic_hook::set_once();
}

/// Compute a frequency table (value_counts) for a string column
/// passed as Arrow IPC bytes.
///
/// Takes: Arrow IPC bytes containing a single string/dictionary column
/// Returns: JSON array of { label, count } sorted by count descending
#[wasm_bindgen]
pub fn value_counts(ipc_bytes: &[u8], column_index: usize) -> Result<JsValue, JsValue> {
    summary::value_counts_impl(ipc_bytes, column_index)
        .map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Compute a histogram (binned counts) for a numeric column.
///
/// Takes: Arrow IPC bytes, column index, number of bins
/// Returns: JSON array of { x0, x1, count }
#[wasm_bindgen]
pub fn histogram(
    ipc_bytes: &[u8],
    column_index: usize,
    num_bins: usize,
) -> Result<JsValue, JsValue> {
    summary::histogram_impl(ipc_bytes, column_index, num_bins)
        .map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Filter rows by a boolean mask and return filtered Arrow IPC bytes.
///
/// Takes: Arrow IPC bytes, boolean mask as Uint8Array (0/1 per row)
/// Returns: Filtered Arrow IPC bytes
#[wasm_bindgen]
pub fn filter_rows(ipc_bytes: &[u8], mask: &[u8]) -> Result<Vec<u8>, JsValue> {
    filter::filter_rows_impl(ipc_bytes, mask).map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Search a string column for values containing a substring.
/// Returns indices of matching rows as a Uint32Array.
///
/// Takes: Arrow IPC bytes, column index, search query
/// Returns: Array of matching row indices
#[wasm_bindgen]
pub fn string_contains(
    ipc_bytes: &[u8],
    column_index: usize,
    query: &str,
) -> Result<Vec<u32>, JsValue> {
    filter::string_contains_impl(ipc_bytes, column_index, query)
        .map_err(|e| JsValue::from_str(&e.to_string()))
}
