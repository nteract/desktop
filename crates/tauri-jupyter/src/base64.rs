//! Base64 serialization/deserialization utilities for Jupyter message buffers.

use base64::prelude::*;
use bytes::Bytes;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Serialize a slice of Bytes as base64-encoded strings.
///
/// Used with `#[serde(serialize_with = "serialize_buffers")]`
pub fn serialize_buffers<S>(data: &[Bytes], serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    data.iter()
        .map(|bytes| BASE64_STANDARD.encode(bytes))
        .collect::<Vec<_>>()
        .serialize(serializer)
}

/// Deserialize base64-encoded buffer strings into Bytes.
///
/// Handles both `null` and missing `buffers` field gracefully,
/// returning an empty Vec in those cases.
///
/// Used with `#[serde(default, deserialize_with = "deserialize_buffers")]`
pub fn deserialize_buffers<'de, D>(deserializer: D) -> Result<Vec<Bytes>, D::Error>
where
    D: Deserializer<'de>,
{
    let encoded: Option<Vec<String>> = Option::deserialize(deserializer)?;
    match encoded {
        Some(vec) => vec
            .iter()
            .map(|s| {
                BASE64_STANDARD
                    .decode(s)
                    .map(Bytes::from)
                    .map_err(serde::de::Error::custom)
            })
            .collect(),
        None => Ok(Vec::new()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    struct TestStruct {
        #[serde(
            default,
            serialize_with = "serialize_buffers",
            deserialize_with = "deserialize_buffers"
        )]
        buffers: Vec<Bytes>,
    }

    #[test]
    fn test_serialize_buffers() {
        let test = TestStruct {
            buffers: vec![Bytes::from("hello"), Bytes::from("world")],
        };
        let json = serde_json::to_string(&test).unwrap();
        assert!(json.contains("aGVsbG8=")); // "hello" in base64
        assert!(json.contains("d29ybGQ=")); // "world" in base64
    }

    #[test]
    fn test_deserialize_buffers() {
        let json = r#"{"buffers": ["aGVsbG8=", "d29ybGQ="]}"#;
        let test: TestStruct = serde_json::from_str(json).unwrap();
        assert_eq!(test.buffers.len(), 2);
        assert_eq!(&test.buffers[0][..], b"hello");
        assert_eq!(&test.buffers[1][..], b"world");
    }

    #[test]
    fn test_deserialize_null_buffers() {
        let json = r#"{"buffers": null}"#;
        let test: TestStruct = serde_json::from_str(json).unwrap();
        assert!(test.buffers.is_empty());
    }

    #[test]
    fn test_deserialize_missing_buffers() {
        let json = r#"{}"#;
        let test: TestStruct = serde_json::from_str(json).unwrap();
        assert!(test.buffers.is_empty());
    }
}
