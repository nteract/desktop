//! Output resolution -- delegates to runtimed-client's canonical implementation.
//!
//! Re-exports are adapted to use the local PyO3 `Output` and `DataValue` types.

use std::collections::HashMap;
use std::path::PathBuf;

use runtime_doc::CommDocEntry;
use runtimed_client::output_resolver as shared;

use crate::output::{DataValue, Output};

/// Convert a shared DataValue to a local (PyO3) DataValue.
fn convert_dv(dv: runtimed_client::resolved_output::DataValue) -> DataValue {
    match dv {
        runtimed_client::resolved_output::DataValue::Text(s) => DataValue::Text(s),
        runtimed_client::resolved_output::DataValue::Binary(b) => DataValue::Binary(b),
        runtimed_client::resolved_output::DataValue::Json(v) => DataValue::Json(v),
    }
}

/// Convert a shared Output to a local (PyO3) Output.
fn convert_output(o: runtimed_client::resolved_output::Output) -> Output {
    Output {
        output_type: o.output_type,
        name: o.name,
        text: o.text,
        data: o
            .data
            .map(|d| d.into_iter().map(|(k, v)| (k, convert_dv(v))).collect()),
        ename: o.ename,
        evalue: o.evalue,
        traceback: o.traceback,
        execution_count: o.execution_count,
        blob_urls: o.blob_urls,
        blob_paths: o.blob_paths,
    }
}

/// Resolve all outputs for a cell snapshot.
pub async fn resolve_cell_outputs(
    raw_outputs: &[serde_json::Value],
    blob_base_url: &Option<String>,
    blob_store_path: &Option<PathBuf>,
    comms: Option<&HashMap<String, CommDocEntry>>,
) -> Vec<Output> {
    shared::resolve_cell_outputs(raw_outputs, blob_base_url, blob_store_path, comms)
        .await
        .into_iter()
        .map(convert_output)
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use runtimed_client::resolved_output as shared_output;
    use serde_json::json;

    #[test]
    fn convert_output_preserves_typed_data_and_blob_metadata() {
        let output = shared_output::Output {
            output_type: "execute_result".to_string(),
            name: None,
            text: None,
            data: Some(HashMap::from([
                (
                    "text/plain".to_string(),
                    shared_output::DataValue::Text("hello".to_string()),
                ),
                (
                    "image/png".to_string(),
                    shared_output::DataValue::Binary(vec![0, 1, 2, 255]),
                ),
                (
                    "application/json".to_string(),
                    shared_output::DataValue::Json(json!({"ok": true})),
                ),
            ])),
            ename: None,
            evalue: None,
            traceback: None,
            execution_count: Some(7),
            blob_urls: Some(HashMap::from([(
                "image/png".to_string(),
                "http://127.0.0.1/blob/image".to_string(),
            )])),
            blob_paths: Some(HashMap::from([(
                "image/png".to_string(),
                "/tmp/runt/blob-image".to_string(),
            )])),
        };

        let converted = convert_output(output);

        assert_eq!(converted.output_type, "execute_result");
        assert_eq!(converted.execution_count, Some(7));
        assert_eq!(
            converted.blob_urls.unwrap()["image/png"],
            "http://127.0.0.1/blob/image"
        );
        assert_eq!(
            converted.blob_paths.unwrap()["image/png"],
            "/tmp/runt/blob-image"
        );

        let data = converted.data.unwrap();
        let DataValue::Text(text) = &data["text/plain"] else {
            panic!("text/plain should convert to text");
        };
        assert_eq!(text, "hello");

        let DataValue::Binary(bytes) = &data["image/png"] else {
            panic!("image/png should convert to binary");
        };
        assert_eq!(bytes, &[0, 1, 2, 255]);

        let DataValue::Json(value) = &data["application/json"] else {
            panic!("application/json should convert to JSON");
        };
        assert_eq!(value["ok"], true);
    }
}
