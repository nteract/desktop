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

    // ── open_notebook tracking ────────────────────────────────────────

    #[test]
    fn tracks_open_notebook_with_path() {
        let params = make_params(
            "open_notebook",
            serde_json::json!({"path": "/tmp/test.ipynb"}),
        );
        assert_eq!(
            extract_session_id(&params, &success_result()),
            Some("/tmp/test.ipynb".to_string())
        );
    }

    #[test]
    fn tracks_open_notebook_with_notebook_id() {
        // open_notebook can also take a notebook_id argument (session UUID)
        let params = make_params(
            "open_notebook",
            serde_json::json!({"notebook_id": "abc-123-def"}),
        );
        assert_eq!(
            extract_session_id(&params, &success_result()),
            Some("abc-123-def".to_string())
        );
    }

    #[test]
    fn prefers_notebook_id_over_path() {
        // When both are present, notebook_id takes precedence
        let params = make_params(
            "open_notebook",
            serde_json::json!({"notebook_id": "abc-123", "path": "/tmp/test.ipynb"}),
        );
        assert_eq!(
            extract_session_id(&params, &success_result()),
            Some("abc-123".to_string())
        );
    }

    // ── create_notebook tracking ──────────────────────────────────────

    #[test]
    fn tracks_create_notebook() {
        let params = make_params(
            "create_notebook",
            serde_json::json!({"path": "/tmp/new.ipynb"}),
        );
        assert_eq!(
            extract_session_id(&params, &success_result()),
            Some("/tmp/new.ipynb".to_string())
        );
    }

    #[test]
    fn tracks_create_notebook_with_notebook_id() {
        let params = make_params(
            "create_notebook",
            serde_json::json!({"notebook_id": "new-uuid"}),
        );
        assert_eq!(
            extract_session_id(&params, &success_result()),
            Some("new-uuid".to_string())
        );
    }

    // ── Tools that should NOT be tracked ──────────────────────────────

    #[test]
    fn ignores_execute_cell() {
        let params = make_params("execute_cell", serde_json::json!({"cell_id": "abc"}));
        assert_eq!(extract_session_id(&params, &success_result()), None);
    }

    #[test]
    fn ignores_save_notebook() {
        let params = make_params(
            "save_notebook",
            serde_json::json!({"path": "/tmp/test.ipynb"}),
        );
        assert_eq!(extract_session_id(&params, &success_result()), None);
    }

    #[test]
    fn ignores_list_active_notebooks() {
        let params = make_params("list_active_notebooks", serde_json::json!({}));
        assert_eq!(extract_session_id(&params, &success_result()), None);
    }

    #[test]
    fn ignores_get_cell() {
        let params = make_params("get_cell", serde_json::json!({"cell_id": "c1"}));
        assert_eq!(extract_session_id(&params, &success_result()), None);
    }

    #[test]
    fn ignores_create_cell() {
        let params = make_params("create_cell", serde_json::json!({"source": "print('hi')"}));
        assert_eq!(extract_session_id(&params, &success_result()), None);
    }

    #[test]
    fn ignores_set_cell() {
        let params = make_params(
            "set_cell",
            serde_json::json!({"cell_id": "c1", "source": "x = 1"}),
        );
        assert_eq!(extract_session_id(&params, &success_result()), None);
    }

    // ── Error handling ────────────────────────────────────────────────

    #[test]
    fn ignores_open_notebook_error() {
        let params = make_params(
            "open_notebook",
            serde_json::json!({"path": "/tmp/test.ipynb"}),
        );
        assert_eq!(extract_session_id(&params, &error_result()), None);
    }

    #[test]
    fn ignores_create_notebook_error() {
        let params = make_params(
            "create_notebook",
            serde_json::json!({"path": "/tmp/new.ipynb"}),
        );
        assert_eq!(extract_session_id(&params, &error_result()), None);
    }

    #[test]
    fn treats_is_error_none_as_success() {
        // is_error = None (not explicitly set) should be treated as success
        let params = make_params(
            "open_notebook",
            serde_json::json!({"path": "/tmp/test.ipynb"}),
        );
        let mut result = CallToolResult::success(vec![Content::text("ok")]);
        // Force is_error to None to test the None case
        result.is_error = None;
        assert_eq!(
            extract_session_id(&params, &result),
            Some("/tmp/test.ipynb".to_string())
        );
    }

    #[test]
    fn treats_is_error_false_as_success() {
        let params = make_params(
            "open_notebook",
            serde_json::json!({"path": "/tmp/test.ipynb"}),
        );
        let mut result = CallToolResult::success(vec![Content::text("ok")]);
        result.is_error = Some(false);
        assert_eq!(
            extract_session_id(&params, &result),
            Some("/tmp/test.ipynb".to_string())
        );
    }

    // ── Edge cases ────────────────────────────────────────────────────

    #[test]
    fn returns_none_when_no_arguments() {
        let params: CallToolRequestParams = serde_json::from_value(serde_json::json!({
            "name": "open_notebook"
        }))
        .unwrap();
        assert_eq!(extract_session_id(&params, &success_result()), None);
    }

    #[test]
    fn returns_none_when_arguments_empty() {
        let params = make_params("open_notebook", serde_json::json!({}));
        assert_eq!(extract_session_id(&params, &success_result()), None);
    }

    #[test]
    fn returns_none_when_path_is_not_string() {
        let params = make_params("open_notebook", serde_json::json!({"path": 42}));
        assert_eq!(extract_session_id(&params, &success_result()), None);
    }

    #[test]
    fn returns_none_when_notebook_id_is_not_string() {
        let params = make_params("open_notebook", serde_json::json!({"notebook_id": true}));
        assert_eq!(extract_session_id(&params, &success_result()), None);
    }

    #[test]
    fn handles_unrelated_arguments() {
        let params = make_params(
            "open_notebook",
            serde_json::json!({"some_other_field": "value"}),
        );
        assert_eq!(extract_session_id(&params, &success_result()), None);
    }

    // ── Deprecated join_notebook is NOT tracked ───────────────────────

    #[test]
    fn does_not_track_join_notebook() {
        let params = make_params(
            "join_notebook",
            serde_json::json!({"notebook_id": "abc-123"}),
        );
        assert_eq!(
            extract_session_id(&params, &success_result()),
            None,
            "join_notebook is deprecated and should not be tracked"
        );
    }
}
