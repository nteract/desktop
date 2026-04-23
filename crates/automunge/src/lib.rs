//! automunge — JSON-to-Automerge helpers.
//!
//! Recursive read, write, and update for `serde_json::Value` in Automerge
//! documents. Used by both `notebook-doc` (NotebookDoc) and `runtime-doc`
//! (RuntimeStateDoc) to avoid duplicating these helpers across crates.
//!
//! Named after automorph (codeberg.org/dpp/automorph), which we may adopt
//! when it ships automerge 0.8 support. Until then, this is the single
//! source of truth for JSON/Automerge conversion.

use automerge::{transaction::Transactable, AutoCommit, AutomergeError, ObjId, ObjType, ReadDoc};

fn scalar_to_json(s: &automerge::ScalarValue) -> Option<serde_json::Value> {
    match s {
        automerge::ScalarValue::Null => Some(serde_json::Value::Null),
        automerge::ScalarValue::Boolean(b) => Some(serde_json::Value::Bool(*b)),
        automerge::ScalarValue::Int(i) => {
            Some(serde_json::Value::Number(serde_json::Number::from(*i)))
        }
        automerge::ScalarValue::Uint(u) => {
            Some(serde_json::Value::Number(serde_json::Number::from(*u)))
        }
        automerge::ScalarValue::F64(f) => Some(
            serde_json::Number::from_f64(*f)
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
        ),
        automerge::ScalarValue::Str(s) => Some(serde_json::Value::String(s.to_string())),
        _ => None,
    }
}

/// Recursively read an Automerge value (scalar, Map, List, or Text) as JSON.
pub fn read_json_value<P: Into<automerge::Prop>>(
    doc: &AutoCommit,
    parent: &ObjId,
    prop: P,
) -> Option<serde_json::Value> {
    let (value, obj_id) = doc.get(parent, prop).ok().flatten()?;
    match value {
        automerge::Value::Scalar(s) => scalar_to_json(s.as_ref()),
        automerge::Value::Object(ObjType::Map) => {
            let mut map = serde_json::Map::new();
            for key in doc.keys(&obj_id) {
                if let Some(v) = read_json_value(doc, &obj_id, key.as_str()) {
                    map.insert(key, v);
                }
            }
            Some(serde_json::Value::Object(map))
        }
        automerge::Value::Object(ObjType::List) => {
            let len = doc.length(&obj_id);
            let arr: Vec<serde_json::Value> = (0..len)
                .map(|i| read_json_value(doc, &obj_id, i).unwrap_or(serde_json::Value::Null))
                .collect();
            Some(serde_json::Value::Array(arr))
        }
        automerge::Value::Object(ObjType::Text) => {
            doc.text(&obj_id).ok().map(serde_json::Value::String)
        }
        _ => None,
    }
}

/// Recursively write a JSON value into an Automerge Map at a string key.
///
/// Creates new `Map`/`List` objects via `put_object`. Dangerous in multi-peer
/// CRDT scenarios: two peers calling `put_object` at the same key produce
/// competing objects. Prefer [`update_json_at_key`] for shared keys.
#[deprecated(
    note = "Use update_json_at_key to avoid put_object conflicts. See nteract/desktop#1594."
)]
#[allow(deprecated)]
pub fn put_json_at_key(
    doc: &mut AutoCommit,
    parent: &ObjId,
    key: &str,
    value: &serde_json::Value,
) -> Result<(), AutomergeError> {
    match value {
        serde_json::Value::Null => {
            doc.put(parent, key, automerge::ScalarValue::Null)?;
        }
        serde_json::Value::Bool(b) => {
            doc.put(parent, key, *b)?;
        }
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                doc.put(parent, key, i)?;
            } else if let Some(u) = n.as_u64() {
                doc.put(parent, key, u)?;
            } else if let Some(f) = n.as_f64() {
                doc.put(parent, key, f)?;
            }
        }
        serde_json::Value::String(s) => {
            doc.put(parent, key, s.as_str())?;
        }
        serde_json::Value::Array(arr) => {
            let list_id = doc.put_object(parent, key, ObjType::List)?;
            for (i, item) in arr.iter().enumerate() {
                insert_json_at_index(doc, &list_id, i, item)?;
            }
        }
        serde_json::Value::Object(map) => {
            let map_id = doc.put_object(parent, key, ObjType::Map)?;
            for (k, v) in map {
                put_json_at_key(doc, &map_id, k, v)?;
            }
        }
    }
    Ok(())
}

/// Recursively insert a JSON value into an Automerge List at a given index.
///
/// Safe when the parent list was just created by the caller (no competing
/// objects possible). For updating existing list elements, use
/// [`update_json_at_index`].
#[allow(deprecated)]
pub fn insert_json_at_index(
    doc: &mut AutoCommit,
    parent: &ObjId,
    index: usize,
    value: &serde_json::Value,
) -> Result<(), AutomergeError> {
    match value {
        serde_json::Value::Null => {
            doc.insert(parent, index, automerge::ScalarValue::Null)?;
        }
        serde_json::Value::Bool(b) => {
            doc.insert(parent, index, *b)?;
        }
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                doc.insert(parent, index, i)?;
            } else if let Some(u) = n.as_u64() {
                doc.insert(parent, index, u)?;
            } else if let Some(f) = n.as_f64() {
                doc.insert(parent, index, f)?;
            }
        }
        serde_json::Value::String(s) => {
            doc.insert(parent, index, s.as_str())?;
        }
        serde_json::Value::Array(arr) => {
            let list_id = doc.insert_object(parent, index, ObjType::List)?;
            for (i, item) in arr.iter().enumerate() {
                insert_json_at_index(doc, &list_id, i, item)?;
            }
        }
        serde_json::Value::Object(map) => {
            let map_id = doc.insert_object(parent, index, ObjType::Map)?;
            for (k, v) in map {
                put_json_at_key(doc, &map_id, k, v)?;
            }
        }
    }
    Ok(())
}

/// Recursively update a JSON value in an Automerge Map, reusing existing objects.
///
/// Unlike [`put_json_at_key`], this looks up existing objects and updates them
/// in-place. Only creates new objects if none exist at the key. This is the
/// read-before-write pattern that avoids `put_object` conflicts in multi-peer
/// scenarios.
pub fn update_json_at_key(
    doc: &mut AutoCommit,
    parent: &ObjId,
    key: &str,
    value: &serde_json::Value,
) -> Result<(), AutomergeError> {
    match value {
        serde_json::Value::Null => {
            doc.put(parent, key, automerge::ScalarValue::Null)?;
        }
        serde_json::Value::Bool(b) => {
            doc.put(parent, key, *b)?;
        }
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                doc.put(parent, key, i)?;
            } else if let Some(u) = n.as_u64() {
                doc.put(parent, key, u)?;
            } else if let Some(f) = n.as_f64() {
                doc.put(parent, key, f)?;
            }
        }
        serde_json::Value::String(s) => {
            doc.put(parent, key, s.as_str())?;
        }
        serde_json::Value::Object(map) => {
            let map_id = match doc.get(parent, key)? {
                Some((automerge::Value::Object(ObjType::Map), id)) => id,
                _ => doc.put_object(parent, key, ObjType::Map)?,
            };
            let existing_keys: Vec<String> = doc.keys(&map_id).collect();
            for old_key in &existing_keys {
                if !map.contains_key(old_key) {
                    let _ = doc.delete(&map_id, old_key.as_str());
                }
            }
            for (k, v) in map {
                update_json_at_key(doc, &map_id, k, v)?;
            }
        }
        serde_json::Value::Array(arr) => {
            let list_id = match doc.get(parent, key)? {
                Some((automerge::Value::Object(ObjType::List), id)) => id,
                _ => doc.put_object(parent, key, ObjType::List)?,
            };
            let existing_len = doc.length(&list_id);
            let new_len = arr.len();
            for (i, item) in arr.iter().enumerate() {
                if i < existing_len {
                    update_json_at_index(doc, &list_id, i, item)?;
                } else {
                    insert_json_at_index(doc, &list_id, i, item)?;
                }
            }
            for i in (new_len..existing_len).rev() {
                let _ = doc.delete(&list_id, i);
            }
        }
    }
    Ok(())
}

/// Recursively update a JSON value at an existing index in an Automerge List,
/// reusing existing objects.
pub fn update_json_at_index(
    doc: &mut AutoCommit,
    parent: &ObjId,
    index: usize,
    value: &serde_json::Value,
) -> Result<(), AutomergeError> {
    match value {
        serde_json::Value::Null => {
            doc.put(parent, index, automerge::ScalarValue::Null)?;
        }
        serde_json::Value::Bool(b) => {
            doc.put(parent, index, *b)?;
        }
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                doc.put(parent, index, i)?;
            } else if let Some(u) = n.as_u64() {
                doc.put(parent, index, u)?;
            } else if let Some(f) = n.as_f64() {
                doc.put(parent, index, f)?;
            }
        }
        serde_json::Value::String(s) => {
            doc.put(parent, index, s.as_str())?;
        }
        serde_json::Value::Object(map) => {
            let map_id = match doc.get(parent, index)? {
                Some((automerge::Value::Object(ObjType::Map), id)) => {
                    let existing_keys: Vec<String> = doc.keys(&id).collect();
                    for old_key in &existing_keys {
                        if !map.contains_key(old_key) {
                            let _ = doc.delete(&id, old_key.as_str());
                        }
                    }
                    id
                }
                _ => {
                    doc.delete(parent, index)?;
                    doc.insert_object(parent, index, ObjType::Map)?
                }
            };
            for (k, v) in map {
                update_json_at_key(doc, &map_id, k, v)?;
            }
        }
        serde_json::Value::Array(arr) => {
            let list_id = match doc.get(parent, index)? {
                Some((automerge::Value::Object(ObjType::List), id)) => {
                    let existing_len = doc.length(&id);
                    for i in (arr.len()..existing_len).rev() {
                        let _ = doc.delete(&id, i);
                    }
                    id
                }
                _ => {
                    doc.delete(parent, index)?;
                    doc.insert_object(parent, index, ObjType::List)?
                }
            };
            let existing_len = doc.length(&list_id);
            for (i, item) in arr.iter().enumerate() {
                if i < existing_len {
                    update_json_at_index(doc, &list_id, i, item)?;
                } else {
                    insert_json_at_index(doc, &list_id, i, item)?;
                }
            }
        }
    }
    Ok(())
}
