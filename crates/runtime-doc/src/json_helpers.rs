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
        _ => None, // Timestamp, Counter, Bytes — not used for JSON metadata
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
/// # Deprecation
///
/// This function creates new `Map`/`List` objects via `put_object`, which is
/// dangerous in multi-peer CRDT scenarios: two peers calling `put_object` at
/// the same key produce competing Automerge objects, and the loser's children
/// become invisible. Use [`update_json_at_key`] instead — it reuses existing
/// objects when possible.
///
/// See <https://github.com/nteract/desktop/issues/1594>.
#[deprecated(
    note = "Use update_json_at_key — put_json_at_key creates new Automerge objects that can conflict with other peers. See #1594."
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
/// # Safety note
///
/// This creates new `Map`/`List` children via `insert_object`, which is safe
/// when the parent list was just created by the caller (no other peer can have
/// a competing object at the same index). However, for updating existing list
/// elements use [`update_json_at_index`] instead.
#[allow(deprecated)] // Internal calls to put_json_at_key are safe — parent just created
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
/// Unlike [`put_json_at_key`] which creates new `Map`/`List` objects (dangerous in
/// multi-peer scenarios), this function looks up existing objects and updates
/// them in-place. Only creates new objects if none exist at the key.
///
/// # Safety contract
///
/// **Conflict-free when target objects already exist** — the common case for
/// shared keys like `metadata.runt`, `metadata.kernelspec`, and comm state.
/// The daemon creates all document structure; clients update existing objects.
///
/// **First-write on absent keys still uses `put_object`** and can conflict if
/// two peers independently create the same absent key. This is acceptable
/// because our architecture guarantees the daemon is the sole structure creator
/// — clients never independently create shared Map/List keys.
///
/// **List element type changes use delete+insert** which can produce duplicates
/// under concurrent modification. In practice, concurrent type changes at the
/// same list position don't occur in our document schema.
///
/// See <https://github.com/nteract/desktop/issues/1594>.
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
            // Reuse existing Map if present, only create if missing or wrong type
            let map_id = match doc.get(parent, key)? {
                Some((automerge::Value::Object(ObjType::Map), id)) => id,
                _ => doc.put_object(parent, key, ObjType::Map)?,
            };
            // Remove stale keys not in the new value
            let existing_keys: Vec<String> = doc.keys(&map_id).collect();
            for old_key in &existing_keys {
                if !map.contains_key(old_key) {
                    let _ = doc.delete(&map_id, old_key.as_str());
                }
            }
            // Recursively update children
            for (k, v) in map {
                update_json_at_key(doc, &map_id, k, v)?;
            }
        }
        serde_json::Value::Array(arr) => {
            // Reuse existing List if present, only create if missing or wrong type
            let list_id = match doc.get(parent, key)? {
                Some((automerge::Value::Object(ObjType::List), id)) => id,
                _ => doc.put_object(parent, key, ObjType::List)?,
            };
            let existing_len = doc.length(&list_id);
            let new_len = arr.len();

            // Update existing elements in-place
            for (i, item) in arr.iter().enumerate() {
                if i < existing_len {
                    update_json_at_index(doc, &list_id, i, item)?;
                } else {
                    insert_json_at_index(doc, &list_id, i, item)?;
                }
            }
            // Remove excess elements from end to avoid index shifting
            for i in (new_len..existing_len).rev() {
                let _ = doc.delete(&list_id, i);
            }
        }
    }
    Ok(())
}

/// Recursively update a JSON value at an existing index in an Automerge List,
/// reusing existing objects.
///
/// For scalars, uses `put()` at the index (last-writer-wins).
/// For Objects/Arrays, reuses existing Automerge objects if possible.
/// If the type at the index doesn't match (e.g. was a scalar, now an object),
/// deletes and re-inserts.
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
            // Reuse existing Map if present at this index
            let map_id = match doc.get(parent, index)? {
                Some((automerge::Value::Object(ObjType::Map), id)) => {
                    // Reuse — remove stale keys
                    let existing_keys: Vec<String> = doc.keys(&id).collect();
                    for old_key in &existing_keys {
                        if !map.contains_key(old_key) {
                            let _ = doc.delete(&id, old_key.as_str());
                        }
                    }
                    id
                }
                _ => {
                    // Type mismatch or missing — delete and re-insert
                    doc.delete(parent, index)?;
                    doc.insert_object(parent, index, ObjType::Map)?
                }
            };
            for (k, v) in map {
                update_json_at_key(doc, &map_id, k, v)?;
            }
        }
        serde_json::Value::Array(arr) => {
            // Reuse existing List if present at this index
            let list_id = match doc.get(parent, index)? {
                Some((automerge::Value::Object(ObjType::List), id)) => {
                    let existing_len = doc.length(&id);
                    // Remove excess elements from end
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
