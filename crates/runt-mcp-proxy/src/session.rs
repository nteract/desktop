//! Notebook session tracking and auto-rejoin after child restart.

use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::Value;
use tracing::{info, warn};

use crate::child::RunningChild;

/// Track notebook_id from session-establishing tool calls.
///
/// When `open_notebook` or `create_notebook` succeeds, returns the notebook_id
/// to persist for auto-rejoin after restarts.
pub fn extract_session_id(
    params: &CallToolRequestParams,
    result: &CallToolResult,
) -> Option<String> {
    // Only track successful calls
    if result.is_error == Some(true) {
        return None;
    }

    let name: &str = &params.name;
    match name {
        "open_notebook" | "create_notebook" => params
            .arguments
            .as_ref()
            .and_then(|args| {
                args.get("notebook_id")
                    .or_else(|| args.get("path"))
                    .and_then(Value::as_str)
            })
            .map(String::from),
        _ => None,
    }
}

/// Attempt to re-join a notebook session in the new child process.
///
/// Returns `true` if rejoin succeeded, `false` otherwise.
pub async fn auto_rejoin(client: &RunningChild, notebook_id: &str) -> bool {
    info!("Auto-rejoining notebook session: {notebook_id}");

    let params: CallToolRequestParams = match serde_json::from_value(serde_json::json!({
        "name": "open_notebook",
        "arguments": { "path": notebook_id }
    })) {
        Ok(p) => p,
        Err(e) => {
            warn!("Failed to build rejoin params: {e}");
            return false;
        }
    };

    match client.call_tool(params).await {
        Ok(result) if result.is_error != Some(true) => {
            info!("Auto-rejoin succeeded for {notebook_id}");
            true
        }
        Ok(_) => {
            warn!("Auto-rejoin returned error for {notebook_id} (notebook may have closed)");
            false
        }
        Err(e) => {
            warn!("Auto-rejoin failed for {notebook_id}: {e}");
            false
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use rmcp::model::Content;

    fn make_params(name: &str, args: serde_json::Value) -> CallToolRequestParams {
        serde_json::from_value(serde_json::json!({
            "name": name,
            "arguments": args
        }))
        .unwrap()
    }

    fn success_result() -> CallToolResult {
        CallToolResult::success(vec![Content::text("ok")])
    }

    fn error_result() -> CallToolResult {
        let mut result = CallToolResult::success(vec![Content::text("error")]);
        result.is_error = Some(true);
        result
    }

    #[test]
    fn tracks_open_notebook() {
        let params = make_params(
            "open_notebook",
            serde_json::json!({"path": "/tmp/test.ipynb"}),
        );
        let result = success_result();
        assert_eq!(
            extract_session_id(&params, &result),
            Some("/tmp/test.ipynb".to_string())
        );
    }

    #[test]
    fn tracks_create_notebook() {
        let params = make_params(
            "create_notebook",
            serde_json::json!({"path": "/tmp/new.ipynb"}),
        );
        let result = success_result();
        assert_eq!(
            extract_session_id(&params, &result),
            Some("/tmp/new.ipynb".to_string())
        );
    }

    #[test]
    fn ignores_other_tools() {
        let params = make_params("execute_cell", serde_json::json!({"cell_id": "abc"}));
        let result = success_result();
        assert_eq!(extract_session_id(&params, &result), None);
    }

    #[test]
    fn ignores_errors() {
        let params = make_params(
            "open_notebook",
            serde_json::json!({"path": "/tmp/test.ipynb"}),
        );
        let result = error_result();
        assert_eq!(extract_session_id(&params, &result), None);
    }
}
