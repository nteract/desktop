//! Tool cache and tool list divergence detection.

use std::collections::HashSet;
use std::path::Path;

use rmcp::model::Tool;
use tracing::{info, warn};

const TOOL_CACHE_FILENAME: &str = "tool-cache.json";

/// Load cached child tool definitions from disk.
pub fn load_cached_tools(cache_dir: &Path) -> Option<Vec<Tool>> {
    let path = cache_dir.join(TOOL_CACHE_FILENAME);
    let data = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<Vec<Tool>>(&data) {
        Ok(tools) => Some(tools),
        Err(e) => {
            warn!("Failed to parse tool cache at {}: {e}", path.display());
            None
        }
    }
}

/// Save child tool definitions to disk for optimistic serving on next startup.
pub fn save_tool_cache(cache_dir: &Path, tools: &[Tool]) {
    let path = cache_dir.join(TOOL_CACHE_FILENAME);
    match serde_json::to_string_pretty(tools) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                warn!("Failed to write tool cache to {}: {e}", path.display());
            } else {
                info!("Saved {} tools to cache at {}", tools.len(), path.display());
            }
        }
        Err(e) => warn!("Failed to serialize tool cache: {e}"),
    }
}

/// Result of comparing old and new tool lists after a child restart.
#[derive(Debug, PartialEq)]
pub enum ToolDivergence {
    /// Tool lists are identical.
    Same,
    /// New tools were added but none removed — safe to continue.
    Superset { added: Vec<String> },
    /// Tools were removed or renamed — MCP client's schema is stale.
    /// The proxy should exit cleanly so the client restarts fresh.
    Incompatible {
        removed: Vec<String>,
        added: Vec<String>,
    },
}

/// Compare tool lists before and after a child restart.
pub fn detect_divergence(old_tools: &[Tool], new_tools: &[Tool]) -> ToolDivergence {
    let old_names: HashSet<&str> = old_tools.iter().map(|t| t.name.as_ref()).collect();
    let new_names: HashSet<&str> = new_tools.iter().map(|t| t.name.as_ref()).collect();

    let removed: Vec<String> = old_names
        .difference(&new_names)
        .map(|s| s.to_string())
        .collect();
    let added: Vec<String> = new_names
        .difference(&old_names)
        .map(|s| s.to_string())
        .collect();

    if removed.is_empty() && added.is_empty() {
        ToolDivergence::Same
    } else if removed.is_empty() {
        ToolDivergence::Superset { added }
    } else {
        ToolDivergence::Incompatible { removed, added }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn tool(name: &str) -> Tool {
        Tool::new(
            name.to_string(),
            format!("Description for {name}"),
            serde_json::Map::new(),
        )
    }

    #[test]
    fn same_tools() {
        let tools = vec![tool("a"), tool("b")];
        assert_eq!(detect_divergence(&tools, &tools), ToolDivergence::Same);
    }

    #[test]
    fn superset_is_safe() {
        let old = vec![tool("a"), tool("b")];
        let new = vec![tool("a"), tool("b"), tool("c")];
        match detect_divergence(&old, &new) {
            ToolDivergence::Superset { added } => {
                assert_eq!(added, vec!["c"]);
            }
            other => panic!("Expected Superset, got {other:?}"),
        }
    }

    #[test]
    fn removal_is_incompatible() {
        let old = vec![tool("a"), tool("b"), tool("c")];
        let new = vec![tool("a"), tool("d")];
        match detect_divergence(&old, &new) {
            ToolDivergence::Incompatible { removed, added } => {
                assert!(removed.contains(&"b".to_string()));
                assert!(removed.contains(&"c".to_string()));
                assert!(added.contains(&"d".to_string()));
            }
            other => panic!("Expected Incompatible, got {other:?}"),
        }
    }
}
