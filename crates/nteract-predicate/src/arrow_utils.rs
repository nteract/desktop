//! Shared Arrow iteration helpers used by both the server-side parquet
//! summarizer and the sift-wasm data store.
//!
//! Arrow's string support has two array types: `StringArray` (Utf8) and
//! `LargeStringArray` (LargeUtf8). Pandas routinely emits LargeUtf8 when
//! writing strings via `to_parquet`, so any code that only downcasts to
//! `StringArray` silently drops those columns. These helpers do the right
//! dispatch once so callers don't each have to remember it.

use arrow::array::{Array, AsArray, LargeStringArray, StringArray, StringViewArray};
use arrow::datatypes::DataType;

use crate::utils::dict_key_at;

/// Like [`for_each_string_indexed`] but passes only the value, no row index.
pub fn for_each_string<F>(column: &dyn Array, mut f: F) -> bool
where
    F: FnMut(&str),
{
    for_each_string_indexed(column, |_, s| f(s))
}

/// Iterate over the string values in a `Utf8`, `LargeUtf8`, or dictionary-
/// encoded-with-string-values column. Calls `f(row_index, value)` for each
/// non-null row. The row index is relative to the column start (not a global
/// offset across batches — callers can add their own offset).
///
/// Returns `true` if the column type was handled, `false` if it wasn't a
/// string-like column (caller should fall back to `ArrayFormatter` or similar).
pub fn for_each_string_indexed<F>(column: &dyn Array, mut f: F) -> bool
where
    F: FnMut(usize, &str),
{
    match column.data_type() {
        DataType::Utf8 => {
            let Some(arr) = column.as_any().downcast_ref::<StringArray>() else {
                return false;
            };
            for i in 0..arr.len() {
                if !arr.is_null(i) {
                    f(i, arr.value(i));
                }
            }
            true
        }
        DataType::LargeUtf8 => {
            let Some(arr) = column.as_any().downcast_ref::<LargeStringArray>() else {
                return false;
            };
            for i in 0..arr.len() {
                if !arr.is_null(i) {
                    f(i, arr.value(i));
                }
            }
            true
        }
        DataType::Utf8View => {
            // Arrow 53+ view arrays; emitted by DuckDB, Polars, and newer
            // parquet writers. Same shape as Utf8 but with inlined short
            // strings — we just iterate values like the other string types.
            let Some(arr) = column.as_any().downcast_ref::<StringViewArray>() else {
                return false;
            };
            for i in 0..arr.len() {
                if !arr.is_null(i) {
                    f(i, arr.value(i));
                }
            }
            true
        }
        DataType::Dictionary(_, _) => {
            let dict_arr = column.as_any_dictionary();
            let keys = dict_arr.keys();
            let values = dict_arr.values();
            if let Some(str_values) = values.as_any().downcast_ref::<StringArray>() {
                for i in 0..keys.len() {
                    if let Some(key) = dict_key_at(keys, i) {
                        f(i, str_values.value(key));
                    }
                }
                true
            } else if let Some(str_values) = values.as_any().downcast_ref::<LargeStringArray>() {
                for i in 0..keys.len() {
                    if let Some(key) = dict_key_at(keys, i) {
                        f(i, str_values.value(key));
                    }
                }
                true
            } else if let Some(str_values) = values.as_any().downcast_ref::<StringViewArray>() {
                for i in 0..keys.len() {
                    if let Some(key) = dict_key_at(keys, i) {
                        f(i, str_values.value(key));
                    }
                }
                true
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Extract a single string value at `row` from a `Utf8`, `LargeUtf8`, or
/// dictionary-encoded-with-string-values column. Returns `None` for null,
/// unhandled types, or a downcast mismatch.
pub fn string_at(column: &dyn Array, row: usize) -> Option<String> {
    if row >= column.len() || column.is_null(row) {
        return None;
    }
    match column.data_type() {
        DataType::Utf8 => column
            .as_any()
            .downcast_ref::<StringArray>()
            .map(|a| a.value(row).to_string()),
        DataType::LargeUtf8 => column
            .as_any()
            .downcast_ref::<LargeStringArray>()
            .map(|a| a.value(row).to_string()),
        DataType::Utf8View => column
            .as_any()
            .downcast_ref::<StringViewArray>()
            .map(|a| a.value(row).to_string()),
        DataType::Dictionary(_, _) => {
            let dict_arr = column.as_any_dictionary();
            let keys = dict_arr.keys();
            let values = dict_arr.values();
            let key = dict_key_at(keys, row)?;
            if let Some(str_values) = values.as_any().downcast_ref::<StringArray>() {
                if key < str_values.len() {
                    return Some(str_values.value(key).to_string());
                }
            } else if let Some(str_values) = values.as_any().downcast_ref::<LargeStringArray>() {
                if key < str_values.len() {
                    return Some(str_values.value(key).to_string());
                }
            } else if let Some(str_values) = values.as_any().downcast_ref::<StringViewArray>() {
                if key < str_values.len() {
                    return Some(str_values.value(key).to_string());
                }
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{DictionaryArray, Int32Array};
    use std::sync::Arc;

    #[test]
    fn iterates_utf8() {
        let arr = StringArray::from(vec![Some("a"), None, Some("b")]);
        let mut collected = Vec::new();
        let handled = for_each_string(&arr, |s| collected.push(s.to_string()));
        assert!(handled);
        assert_eq!(collected, vec!["a", "b"]);
    }

    #[test]
    fn iterates_large_utf8() {
        let arr = LargeStringArray::from(vec![Some("x"), Some("y")]);
        let mut collected = Vec::new();
        let handled = for_each_string(&arr, |s| collected.push(s.to_string()));
        assert!(handled);
        assert_eq!(collected, vec!["x", "y"]);
    }

    #[test]
    fn iterates_dictionary() {
        let keys = Int32Array::from(vec![Some(0), Some(1), Some(0)]);
        let values = Arc::new(StringArray::from(vec!["foo", "bar"])) as _;
        let dict = DictionaryArray::try_new(keys, values).unwrap();
        let mut collected = Vec::new();
        let handled = for_each_string(&dict, |s| collected.push(s.to_string()));
        assert!(handled);
        assert_eq!(collected, vec!["foo", "bar", "foo"]);
    }

    #[test]
    fn iterates_dictionary_with_int64_keys_and_large_string_values() {
        // Polars/pandas often emit DictionaryArray<Int64, LargeUtf8>. Both the
        // key-width dispatch (via dict_key_at) and the LargeStringArray value
        // downcast need to work.
        use arrow::array::Int64Array;
        let keys = Int64Array::from(vec![Some(1_i64), Some(0), Some(1)]);
        let values = Arc::new(LargeStringArray::from(vec!["apple", "banana"])) as _;
        let dict = DictionaryArray::try_new(keys, values).unwrap();
        let mut collected = Vec::new();
        let handled = for_each_string(&dict, |s| collected.push(s.to_string()));
        assert!(handled);
        assert_eq!(collected, vec!["banana", "apple", "banana"]);
    }

    #[test]
    fn iterates_utf8_view() {
        // Arrow 53+ view array — emitted by DuckDB and newer parquet writers.
        use arrow::array::StringViewArray;
        let arr = StringViewArray::from(vec![Some("short"), None, Some("also-short")]);
        let mut collected = Vec::new();
        let handled = for_each_string(&arr, |s| collected.push(s.to_string()));
        assert!(handled);
        assert_eq!(collected, vec!["short", "also-short"]);
    }

    #[test]
    fn returns_false_for_unknown_type() {
        let arr = Int32Array::from(vec![1, 2, 3]);
        let handled = for_each_string(&arr, |_| {});
        assert!(!handled);
    }

    #[test]
    fn string_at_utf8_and_large_utf8() {
        let small = StringArray::from(vec![Some("a"), None, Some("b")]);
        assert_eq!(string_at(&small, 0), Some("a".to_string()));
        assert_eq!(string_at(&small, 1), None);
        assert_eq!(string_at(&small, 2), Some("b".to_string()));

        let large = LargeStringArray::from(vec![Some("hello")]);
        assert_eq!(string_at(&large, 0), Some("hello".to_string()));
    }
}
