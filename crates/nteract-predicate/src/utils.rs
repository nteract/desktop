use arrow::array::Array;
use arrow::datatypes::DataType;

/// Extract a dictionary key at index `i` as `usize`, handling all integer key types
/// (Int8/16/32/64, UInt8/16/32/64). Returns `None` if the key is null or unsupported.
pub(crate) fn dict_key_at(keys: &dyn Array, i: usize) -> Option<usize> {
    if keys.is_null(i) {
        return None;
    }
    match keys.data_type() {
        DataType::Int8 => keys
            .as_any()
            .downcast_ref::<arrow::array::Int8Array>()
            .map(|a| a.value(i) as usize),
        DataType::Int16 => keys
            .as_any()
            .downcast_ref::<arrow::array::Int16Array>()
            .map(|a| a.value(i) as usize),
        DataType::Int32 => keys
            .as_any()
            .downcast_ref::<arrow::array::Int32Array>()
            .map(|a| a.value(i) as usize),
        DataType::Int64 => keys
            .as_any()
            .downcast_ref::<arrow::array::Int64Array>()
            .map(|a| a.value(i) as usize),
        DataType::UInt8 => keys
            .as_any()
            .downcast_ref::<arrow::array::UInt8Array>()
            .map(|a| a.value(i) as usize),
        DataType::UInt16 => keys
            .as_any()
            .downcast_ref::<arrow::array::UInt16Array>()
            .map(|a| a.value(i) as usize),
        DataType::UInt32 => keys
            .as_any()
            .downcast_ref::<arrow::array::UInt32Array>()
            .map(|a| a.value(i) as usize),
        DataType::UInt64 => keys
            .as_any()
            .downcast_ref::<arrow::array::UInt64Array>()
            .map(|a| a.value(i) as usize),
        _ => None,
    }
}
