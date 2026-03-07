//! Code formatting for notebook cells.
//!
//! Supports:
//! - Python via `ruff format` (auto-bootstrapped via rattler if not on PATH)
//! - TypeScript/JavaScript via `deno fmt` (auto-bootstrapped via rattler if not on PATH)

use crate::tools;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

/// Result of a format operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatResult {
    /// The formatted source code
    pub source: String,
    /// Whether formatting changed the source
    pub changed: bool,
    /// Error message if formatting failed (source unchanged)
    pub error: Option<String>,
}

impl FormatResult {
    /// Strip the trailing newline that formatters (ruff, deno) always add.
    /// Notebook cells should not end with a trailing newline per nbformat convention.
    pub fn source_for_cell(&self) -> &str {
        self.source.strip_suffix('\n').unwrap_or(&self.source)
    }
}

/// Check if ruff is available (either on PATH or bootstrappable via rattler)
pub async fn check_ruff_available() -> bool {
    tools::get_ruff_path().await.is_ok()
}

/// Format Python code using ruff
///
/// Ruff is auto-bootstrapped via rattler if not found on PATH.
pub async fn format_python(source: &str) -> Result<FormatResult> {
    // Skip formatting for empty or whitespace-only source
    if source.trim().is_empty() {
        return Ok(FormatResult {
            source: source.to_string(),
            changed: false,
            error: None,
        });
    }

    // Get ruff path (from PATH or bootstrapped via rattler)
    let ruff_path = tools::get_ruff_path().await?;

    let mut child = tokio::process::Command::new(&ruff_path)
        .args(["format", "--stdin-filename", "cell.py", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow!("Failed to spawn ruff: {}", e))?;

    // Write source to stdin
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(source.as_bytes())
            .await
            .map_err(|e| anyhow!("Failed to write to ruff stdin: {}", e))?;
    }

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| anyhow!("Failed to wait for ruff: {}", e))?;

    if output.status.success() {
        let formatted = String::from_utf8(output.stdout)
            .map_err(|e| anyhow!("Invalid UTF-8 in ruff output: {}", e))?;
        let changed = formatted != source;
        Ok(FormatResult {
            source: formatted,
            changed,
            error: None,
        })
    } else {
        // Formatting failed (e.g., syntax error) - return original source with error
        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(FormatResult {
            source: source.to_string(),
            changed: false,
            error: Some(stderr.to_string()),
        })
    }
}

/// Format TypeScript/JavaScript code using deno fmt
///
/// Deno is auto-bootstrapped via rattler if not found on PATH.
pub async fn format_deno(source: &str, language: &str) -> Result<FormatResult> {
    // Skip formatting for empty or whitespace-only source
    if source.trim().is_empty() {
        return Ok(FormatResult {
            source: source.to_string(),
            changed: false,
            error: None,
        });
    }

    let ext = match language {
        "typescript" | "ts" => "ts",
        "javascript" | "js" => "js",
        "tsx" => "tsx",
        "jsx" => "jsx",
        _ => "ts", // default to TypeScript for Deno notebooks
    };

    // Get deno path (from PATH or bootstrapped via rattler)
    let deno_path = tools::get_deno_path().await?;

    let mut child = tokio::process::Command::new(&deno_path)
        .args(["fmt", &format!("--ext={}", ext), "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow!("Failed to spawn deno fmt: {}", e))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(source.as_bytes())
            .await
            .map_err(|e| anyhow!("Failed to write to deno fmt stdin: {}", e))?;
    }

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| anyhow!("Failed to wait for deno fmt: {}", e))?;

    if output.status.success() {
        let formatted = String::from_utf8(output.stdout)
            .map_err(|e| anyhow!("Invalid UTF-8 in deno fmt output: {}", e))?;
        let changed = formatted != source;
        Ok(FormatResult {
            source: formatted,
            changed,
            error: None,
        })
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(FormatResult {
            source: source.to_string(),
            changed: false,
            error: Some(stderr.to_string()),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_format_python_empty() {
        let result = format_python("").await.unwrap();
        assert!(!result.changed);
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn test_format_python_whitespace() {
        let result = format_python("   \n\n  ").await.unwrap();
        assert!(!result.changed);
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn test_format_deno_empty() {
        let result = format_deno("", "ts").await.unwrap();
        assert!(!result.changed);
        assert!(result.error.is_none());
    }

    // Integration tests that require ruff/deno to be installed
    #[tokio::test]
    #[ignore] // Run with --ignored to test with actual formatters
    async fn test_format_python_simple() {
        let source = "x=1\ny  =  2";
        let result = format_python(source).await.unwrap();
        assert!(result.changed);
        assert!(result.source.contains("x = 1"));
    }

    #[tokio::test]
    #[ignore] // Run with --ignored to test with actual formatters
    async fn test_format_deno_typescript() {
        let source = "const x=1;const y  =  2";
        let result = format_deno(source, "ts").await.unwrap();
        assert!(result.changed);
        assert!(result.source.contains("const x = 1"));
    }
}
