//! PEP 723 inline script metadata parser.
//!
//! Extracts `# /// script` blocks from Python source code per
//! <https://peps.python.org/pep-0723/>. This module is pure computation
//! with no I/O — it compiles to WASM, native, and PyO3 targets.
//!
//! ## Usage
//!
//! ```rust
//! use notebook_doc::pep723::{find_pep723_in_sources, Pep723Metadata};
//!
//! // Scan cell sources for a PEP 723 script block
//! let sources = vec!["# /// script\n# dependencies = [\"requests\"]\n# ///"];
//! let meta = find_pep723_in_sources(&sources).unwrap().unwrap();
//! assert_eq!(meta.dependencies, vec!["requests"]);
//! ```

use serde::Deserialize;

/// Parsed PEP 723 inline script metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct Pep723Metadata {
    /// PEP 508 dependency specifiers (e.g. `["pandas>=2.0", "numpy"]`).
    pub dependencies: Vec<String>,
    /// Python version constraint (e.g. `">=3.11"`).
    pub requires_python: Option<String>,
}

/// Error returned when PEP 723 extraction fails.
#[derive(Debug, Clone, PartialEq)]
pub enum Pep723Error {
    /// Multiple `# /// script` blocks found. PEP 723 requires exactly one.
    DuplicateScriptBlock,
    /// The TOML content inside the block is malformed.
    InvalidToml(String),
}

impl std::fmt::Display for Pep723Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Pep723Error::DuplicateScriptBlock => {
                write!(
                    f,
                    "multiple # /// script blocks found (PEP 723 requires exactly one)"
                )
            }
            Pep723Error::InvalidToml(msg) => write!(f, "invalid TOML in script block: {}", msg),
        }
    }
}

impl std::error::Error for Pep723Error {}

// ── Internal TOML schema ────────────────────────────────────────────────

/// TOML structure inside a `# /// script` block.
#[derive(Deserialize)]
struct ScriptToml {
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(rename = "requires-python")]
    requires_python: Option<String>,
}

// ── Block extraction ────────────────────────────────────────────────────

/// Extract the raw TOML content from a `# /// script` block in Python source.
///
/// Returns `None` if no block is found. Returns `Err` if multiple blocks
/// of type `script` are found (per PEP 723 spec, tools MUST error).
///
/// The parser is intentionally simple — line-by-line, no regex dependency.
/// This keeps the WASM binary small.
fn extract_script_block(source: &str) -> Result<Option<String>, Pep723Error> {
    let mut found: Option<String> = None;
    let mut in_block = false;
    let mut content_lines: Vec<String> = Vec::new();

    for line in source.lines() {
        if !in_block {
            // Check for start marker: exactly "# /// script"
            let trimmed = line.trim_end();
            if trimmed == "# /// script" {
                if found.is_some() {
                    return Err(Pep723Error::DuplicateScriptBlock);
                }
                in_block = true;
                content_lines.clear();
            }
        } else {
            let trimmed = line.trim_end();
            // Check for end marker: exactly "# ///"
            if trimmed == "# ///" {
                in_block = false;
                found = Some(content_lines.join("\n"));
                content_lines.clear();
            } else if let Some(rest) = trimmed.strip_prefix("# ") {
                // Content line with text: strip "# " prefix
                content_lines.push(rest.to_string());
            } else if trimmed == "#" {
                // Blank content line (lone #)
                content_lines.push(String::new());
            } else {
                // Line doesn't start with # — malformed block, treat as unclosed
                // Per spec: "If a line does not start with #, the block is not valid"
                // We abandon this block (unclosed blocks are ignored)
                in_block = false;
                content_lines.clear();
            }
        }
    }

    // If we're still in_block at EOF, the block is unclosed → ignore it
    Ok(found)
}

// ── Public API ──────────────────────────────────────────────────────────

/// Extract PEP 723 metadata from a single Python source string.
///
/// Returns `None` if no `# /// script` block is found.
pub fn parse_pep723(source: &str) -> Result<Option<Pep723Metadata>, Pep723Error> {
    let toml_content = match extract_script_block(source)? {
        Some(content) => content,
        None => return Ok(None),
    };

    let script: ScriptToml =
        toml::from_str(&toml_content).map_err(|e| Pep723Error::InvalidToml(e.to_string()))?;

    Ok(Some(Pep723Metadata {
        dependencies: script.dependencies,
        requires_python: script.requires_python,
    }))
}

/// Scan multiple cell sources for a PEP 723 `# /// script` block.
///
/// Only code cell sources should be passed — markdown/raw cells are the
/// caller's responsibility to filter.
///
/// Returns:
/// - `Ok(Some(meta))` if exactly one block is found across all cells
/// - `Ok(None)` if no block is found
/// - `Err(DuplicateScriptBlock)` if multiple blocks are found (across
///   cells or within a single cell)
pub fn find_pep723_in_sources(sources: &[&str]) -> Result<Option<Pep723Metadata>, Pep723Error> {
    let mut result: Option<Pep723Metadata> = None;

    for source in sources {
        if let Some(meta) = parse_pep723(source)? {
            if result.is_some() {
                return Err(Pep723Error::DuplicateScriptBlock);
            }
            result = Some(meta);
        }
    }

    Ok(result)
}

/// Convenience: scan `CellSnapshot` slices from an Automerge doc.
///
/// Filters to code cells automatically.
pub fn find_pep723_in_cells(
    cells: &[crate::CellSnapshot],
) -> Result<Option<Pep723Metadata>, Pep723Error> {
    let sources: Vec<&str> = cells
        .iter()
        .filter(|c| c.cell_type == "code")
        .map(|c| c.source.as_str())
        .collect();

    find_pep723_in_sources(&sources)
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_extraction() {
        let source = r#"# /// script
# dependencies = ["requests", "rich"]
# requires-python = ">=3.11"
# ///"#;

        let meta = parse_pep723(source).unwrap().unwrap();
        assert_eq!(meta.dependencies, vec!["requests", "rich"]);
        assert_eq!(meta.requires_python, Some(">=3.11".to_string()));
    }

    #[test]
    fn test_no_block() {
        let source = "print('hello')\nx = 1 + 2\n";
        assert_eq!(parse_pep723(source).unwrap(), None);
    }

    #[test]
    fn test_empty_dependencies() {
        let source = "# /// script\n# dependencies = []\n# ///\n";
        let meta = parse_pep723(source).unwrap().unwrap();
        assert!(meta.dependencies.is_empty());
        assert_eq!(meta.requires_python, None);
    }

    #[test]
    fn test_deps_only_no_requires_python() {
        let source = "# /// script\n# dependencies = [\"httpx\"]\n# ///";
        let meta = parse_pep723(source).unwrap().unwrap();
        assert_eq!(meta.dependencies, vec!["httpx"]);
        assert_eq!(meta.requires_python, None);
    }

    #[test]
    fn test_block_with_surrounding_code() {
        let source = r#"import os

# /// script
# dependencies = ["pandas>=2.0"]
# ///

print(os.getcwd())"#;

        let meta = parse_pep723(source).unwrap().unwrap();
        assert_eq!(meta.dependencies, vec!["pandas>=2.0"]);
    }

    #[test]
    fn test_duplicate_blocks_in_single_source() {
        let source = "# /// script\n# dependencies = [\"a\"]\n# ///\n# /// script\n# dependencies = [\"b\"]\n# ///\n";
        let err = parse_pep723(source).unwrap_err();
        assert_eq!(err, Pep723Error::DuplicateScriptBlock);
    }

    #[test]
    fn test_duplicate_blocks_across_cells() {
        let cell1 = "# /// script\n# dependencies = [\"a\"]\n# ///";
        let cell2 = "# /// script\n# dependencies = [\"b\"]\n# ///";
        let err = find_pep723_in_sources(&[cell1, cell2]).unwrap_err();
        assert_eq!(err, Pep723Error::DuplicateScriptBlock);
    }

    #[test]
    fn test_unclosed_block_ignored() {
        let _source = "# /// script\n# dependencies = [\"a\"]\n# no closing marker\n";
        // Unclosed block — the non-comment line breaks the block, then EOF
        // Actually this line starts with #, so it's still in the block.
        // Let's use a truly unclosed block (EOF without # ///):
        let source2 = "# /// script\n# dependencies = [\"a\"]\n";
        assert_eq!(parse_pep723(source2).unwrap(), None);
    }

    #[test]
    fn test_non_comment_line_breaks_block() {
        // A line that doesn't start with # inside a block makes it invalid
        let source = "# /// script\n# dependencies = [\"a\"]\nprint('oops')\n# ///\n";
        assert_eq!(parse_pep723(source).unwrap(), None);
    }

    #[test]
    fn test_blank_lines_in_toml() {
        let source = "# /// script\n# dependencies = [\n#   \"requests\",\n#   \"rich\",\n# ]\n#\n# requires-python = \">=3.11\"\n# ///";
        let meta = parse_pep723(source).unwrap().unwrap();
        assert_eq!(meta.dependencies, vec!["requests", "rich"]);
        assert_eq!(meta.requires_python, Some(">=3.11".to_string()));
    }

    #[test]
    fn test_non_script_type_ignored() {
        // A block with a different type name should be ignored
        let source = "# /// something-else\n# key = \"value\"\n# ///\n";
        assert_eq!(parse_pep723(source).unwrap(), None);
    }

    #[test]
    fn test_invalid_toml() {
        let source = "# /// script\n# this is not valid toml {{{\n# ///";
        let err = parse_pep723(source).unwrap_err();
        assert!(matches!(err, Pep723Error::InvalidToml(_)));
    }

    #[test]
    fn test_multiline_deps_with_version_specs() {
        let source = r#"# /// script
# requires-python = ">=3.10"
# dependencies = [
#   "requests<3",
#   "rich>=13.0",
#   "click~=8.0",
# ]
# ///"#;

        let meta = parse_pep723(source).unwrap().unwrap();
        assert_eq!(
            meta.dependencies,
            vec!["requests<3", "rich>=13.0", "click~=8.0"]
        );
        assert_eq!(meta.requires_python, Some(">=3.10".to_string()));
    }

    #[test]
    fn test_tool_section_ignored() {
        // [tool] is valid PEP 723 TOML but we don't parse it
        let source =
            "# /// script\n# dependencies = [\"httpx\"]\n# [tool.ruff]\n# line-length = 88\n# ///";
        let meta = parse_pep723(source).unwrap().unwrap();
        assert_eq!(meta.dependencies, vec!["httpx"]);
    }

    /// Helper to build a CellSnapshot for tests without boilerplate.
    fn cell(id: &str, cell_type: &str, source: &str, position: &str) -> crate::CellSnapshot {
        crate::CellSnapshot {
            id: id.to_string(),
            cell_type: cell_type.to_string(),
            source: source.to_string(),
            position: position.to_string(),
            execution_count: "null".to_string(),
            metadata: serde_json::json!({}),
            resolved_assets: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn test_find_in_cells() {
        let cells = vec![
            cell(
                "1",
                "code",
                "# /// script\n# dependencies = [\"httpx\"]\n# ///",
                "a",
            ),
            cell("2", "code", "print('hello')", "b"),
        ];

        let meta = find_pep723_in_cells(&cells).unwrap().unwrap();
        assert_eq!(meta.dependencies, vec!["httpx"]);
    }

    #[test]
    fn test_markdown_cells_skipped() {
        let cells = vec![
            cell(
                "1",
                "markdown",
                "# /// script\n# dependencies = [\"httpx\"]\n# ///",
                "a",
            ),
            cell("2", "code", "print('hello')", "b"),
        ];

        assert_eq!(find_pep723_in_cells(&cells).unwrap(), None);
    }

    #[test]
    fn test_juv_style_hidden_cell() {
        // juv convention: first code cell with source_hidden metadata
        // We don't care about the metadata — just the source content
        let source = "# /// script\n# requires-python = \">=3.11\"\n# dependencies = [\n#   \"requests<3\",\n#   \"rich\",\n# ]\n# ///";

        let cells = vec![
            cell("meta", "code", source, "a"),
            cell(
                "work",
                "code",
                "import requests\nprint(requests.get('https://example.com').status_code)",
                "b",
            ),
        ];

        let meta = find_pep723_in_cells(&cells).unwrap().unwrap();
        assert_eq!(meta.dependencies, vec!["requests<3", "rich"]);
        assert_eq!(meta.requires_python, Some(">=3.11".to_string()));
    }

    #[test]
    fn test_empty_source() {
        assert_eq!(parse_pep723("").unwrap(), None);
    }

    #[test]
    fn test_only_start_marker() {
        // Just the start marker, no end — unclosed, ignored
        assert_eq!(parse_pep723("# /// script").unwrap(), None);
    }

    #[test]
    fn test_empty_block() {
        // Valid block with no content — empty TOML
        let source = "# /// script\n# ///";
        let meta = parse_pep723(source).unwrap().unwrap();
        assert!(meta.dependencies.is_empty());
        assert_eq!(meta.requires_python, None);
    }

    #[test]
    fn test_trailing_whitespace_on_markers() {
        // Markers with trailing spaces should still match (we trim_end)
        let source = "# /// script   \n# dependencies = [\"a\"]\n# ///  \n";
        let meta = parse_pep723(source).unwrap().unwrap();
        assert_eq!(meta.dependencies, vec!["a"]);
    }
}
