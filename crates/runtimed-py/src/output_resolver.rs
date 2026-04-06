//! Output resolution -- delegates to runtimed-client's canonical implementation.
//!
//! Re-exports are adapted to use the local PyO3 `Output` and `DataValue` types.

use std::collections::HashMap;
use std::path::PathBuf;

use notebook_doc::runtime_state::CommDocEntry;
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
