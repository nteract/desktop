//! Notebook session tracking and auto-rejoin after child restart.

use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::Value;
use tracing::{info, warn};

use crate::child::RunningChild;

/// Track notebook_id from session-establishing tool calls.
///
/// When `open_notebook` or `create_notebook` succeeds, returns the notebook_id
/// to persist for auto-rejoin after restarts.
///
/// Checks request arguments first (open_notebook passes path/notebook_id),
/// then falls back to parsing the response content (create_notebook returns
/// notebook_id in its JSON response, not in request args).
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
        "open_notebook" | "create_notebook" => {
            // Try request arguments first (open_notebook)
            let from_args = params
                .arguments
                .as_ref()
                .and_then(|args| {
                    args.get("notebook_id")
                        .or_else(|| args.get("path"))
                        .and_then(Value::as_str)
                })
                .map(String::from);

            if from_args.is_some() {
                return from_args;
            }

            // Fall back to parsing the response content (create_notebook returns
            // notebook_id in its JSON text response body)
            extract_notebook_id_from_result(result)
        }
        _ => None,
    }
}

/// Parse notebook_id from a tool result's text content (JSON response body).
fn extract_notebook_id_from_result(result: &CallToolResult) -> Option<String> {
    for content in &result.content {
        if let Some(text) = content.raw.as_text() {
            if let Ok(json) = serde_json::from_str::<Value>(&text.text) {
                if let Some(id) = json.get("notebook_id").and_then(Value::as_str) {
                    return Some(id.to_string());
                }
            }
        }
    }
    None
}

fn looks_like_untitled_notebook_id(target: &str) -> bool {
    let path = std::path::Path::new(target);
    path.components().count() == 1
        && path.extension().is_none()
        && uuid::Uuid::parse_str(target).is_ok()
}

fn build_rejoin_params(target: &str) -> Option<CallToolRequestParams> {
    let arguments = if looks_like_untitled_notebook_id(target) {
        serde_json::json!({ "notebook_id": target })
    } else {
        serde_json::json!({ "path": target })
    };

    serde_json::from_value(serde_json::json!({
        "name": "open_notebook",
        "arguments": arguments
    }))
    .ok()
}

/// Attempt to re-join a notebook session in the new child process.
///
/// Returns `true` if rejoin succeeded, `false` otherwise.
pub async fn auto_rejoin(client: &RunningChild, notebook_id: &str) -> bool {
    info!("Auto-rejoining notebook session: {notebook_id}");

    let params = match build_rejoin_params(notebook_id) {
        Some(p) => p,
        None => {
            let e = "invalid auto-rejoin parameters";
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

    #[test]
    fn build_rejoin_params_uses_notebook_id_for_uuid_targets() {
        let target = "550e8400-e29b-41d4-a716-446655440000";
        let params = build_rejoin_params(target).expect("params");
        let args = params.arguments.expect("args");

        assert_eq!(
            args.get("notebook_id").and_then(Value::as_str),
            Some(target)
        );
        assert!(args.get("path").is_none());
    }

    #[test]
    fn build_rejoin_params_uses_path_for_file_targets() {
        let target = "/tmp/test.ipynb";
        let params = build_rejoin_params(target).expect("params");
        let args = params.arguments.expect("args");

        assert_eq!(args.get("path").and_then(Value::as_str), Some(target));
        assert!(args.get("notebook_id").is_none());
    }

    // ── create_notebook tracking ──────────────────────────────────────

    #[test]
    fn tracks_create_notebook_with_path_arg() {
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
    fn tracks_create_notebook_with_notebook_id_arg() {
        let params = make_params(
            "create_notebook",
            serde_json::json!({"notebook_id": "new-uuid"}),
        );
        assert_eq!(
            extract_session_id(&params, &success_result()),
            Some("new-uuid".to_string())
        );
    }

    #[test]
    fn tracks_create_notebook_from_response() {
        // create_notebook typically has no path/notebook_id in args —
        // the notebook_id is in the JSON response body
        let params = make_params("create_notebook", serde_json::json!({}));
        let result = CallToolResult::success(vec![Content::text(
            r#"{"notebook_id": "8540eb53-8609-471d-88f4-5c3e92c3b396", "runtime": {"language": "python"}}"#,
        )]);
        assert_eq!(
            extract_session_id(&params, &result),
            Some("8540eb53-8609-471d-88f4-5c3e92c3b396".to_string())
        );
    }

    #[test]
    fn tracks_create_notebook_with_deps_from_response() {
        // Real-world create_notebook: deps in args, notebook_id in response
        let params = make_params(
            "create_notebook",
            serde_json::json!({"dependencies": ["numpy", "pandas"]}),
        );
        let result = CallToolResult::success(vec![Content::text(
            r#"{"notebook_id": "abc-123", "runtime": {"language": "python"}, "dependencies": ["numpy", "pandas"], "package_manager": "uv"}"#,
        )]);
        assert_eq!(
            extract_session_id(&params, &result),
            Some("abc-123".to_string())
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
    fn returns_none_when_arguments_empty_and_no_response_id() {
        let params = make_params("open_notebook", serde_json::json!({}));
        // success_result() has "ok" text, not JSON with notebook_id
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
}
