//! Pattern resolution for replace_match and replace_regex tools.
//!
//! Ports the Python `_editing.py` logic to Rust. The key contract:
//! - `replace_match`: exact literal match with optional context disambiguation
//! - `replace_regex`: Python-compatible regex with MULTILINE flag
//!
//! Both require exactly one match to succeed.

/// An edit span in the source text (byte offsets).
#[derive(Debug)]
pub struct EditSpan {
    pub start: usize,
    pub end: usize,
}

/// Result of a successful edit.
pub struct EditResult {
    /// The new source text after replacement.
    pub new_source: String,
    /// The span that was replaced (byte offsets in original source).
    pub span: EditSpan,
}

/// Error from pattern resolution.
#[derive(Debug)]
pub enum EditError {
    /// No matches found.
    NoMatch(String),
    /// Multiple matches found — need context to disambiguate.
    AmbiguousMatch { count: usize, offsets: Vec<usize> },
    /// Invalid regex pattern.
    InvalidPattern(String),
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoMatch(msg) => write!(f, "{msg}"),
            Self::AmbiguousMatch { count, offsets } => {
                write!(
                    f,
                    "Found {count} matches (at byte offsets {offsets:?}). Provide context_before/context_after to disambiguate."
                )
            }
            Self::InvalidPattern(msg) => write!(f, "Invalid regex: {msg}"),
        }
    }
}

/// Resolve a literal match with optional context disambiguation.
///
/// Finds all occurrences of `match_text`, then filters by `context_before`
/// and `context_after` proximity. Context strings are searched within a
/// window around each match (same line + nearby lines), not required to be
/// immediately adjacent.
///
/// Returns the byte span of the unique match, or an error.
pub fn resolve_match(
    source: &str,
    match_text: &str,
    context_before: Option<&str>,
    context_after: Option<&str>,
) -> Result<EditSpan, EditError> {
    if match_text.is_empty() {
        return Err(EditError::NoMatch("Match text cannot be empty".to_string()));
    }

    // Find all occurrences of the literal match text
    let escaped_match = regex::escape(match_text);
    let re =
        regex::Regex::new(&escaped_match).map_err(|e| EditError::InvalidPattern(e.to_string()))?;
    let all_matches: Vec<regex::Match> = re.find_iter(source).collect();

    if all_matches.is_empty() {
        return Err(EditError::NoMatch(format!(
            "No match found for '{match_text}'"
        )));
    }

    // If no context provided, require exactly one match
    if context_before.is_none() && context_after.is_none() {
        return match all_matches.len() {
            1 => {
                let m = &all_matches[0];
                Ok(EditSpan {
                    start: m.start(),
                    end: m.end(),
                })
            }
            n => Err(EditError::AmbiguousMatch {
                count: n,
                offsets: all_matches.iter().map(|m| m.start()).collect(),
            }),
        };
    }

    // For each match, search for context only in the "gap" between
    // adjacent matches of the target text. This scopes context to the
    // region that uniquely belongs to each match occurrence.
    let filtered: Vec<&regex::Match> = all_matches
        .iter()
        .enumerate()
        .filter(|(i, m)| {
            if let Some(before) = context_before {
                let gap_start = if *i > 0 { all_matches[i - 1].end() } else { 0 };
                let before_text = &source[gap_start..m.start()];
                if !before_text.contains(before) {
                    return false;
                }
            }
            if let Some(after) = context_after {
                let gap_end = if *i + 1 < all_matches.len() {
                    all_matches[i + 1].start()
                } else {
                    source.len()
                };
                let after_text = &source[m.end()..gap_end];
                if !after_text.contains(after) {
                    return false;
                }
            }
            true
        })
        .map(|(_, m)| m)
        .collect();

    match filtered.len() {
        0 => Err(EditError::NoMatch(format!(
            "No match found for '{match_text}'"
        ))),
        1 => {
            let m = filtered[0];
            Ok(EditSpan {
                start: m.start(),
                end: m.end(),
            })
        }
        n => Err(EditError::AmbiguousMatch {
            count: n,
            offsets: filtered.iter().map(|m| m.start()).collect(),
        }),
    }
}

/// Resolve a regex pattern (Python re.MULTILINE equivalent).
///
/// Uses `fancy_regex` to support lookarounds (`(?=...)`, `(?<=...)`, etc.)
/// that the standard `regex` crate does not handle.
///
/// Returns the byte span of the unique match, or an error.
pub fn resolve_regex(source: &str, pattern: &str) -> Result<EditSpan, EditError> {
    // Wrap the user pattern with (?m) for multiline mode (^ and $ match line boundaries).
    let ml_pattern = format!("(?m){pattern}");
    let re = fancy_regex::Regex::new(&ml_pattern)
        .map_err(|e| EditError::InvalidPattern(e.to_string()))?;

    // fancy_regex::Regex::find_iter returns Result<Match>, not Match
    let matches: Vec<fancy_regex::Match> = re.find_iter(source).filter_map(|m| m.ok()).collect();

    match matches.len() {
        0 => Err(EditError::NoMatch(format!(
            "No match for pattern /{pattern}/"
        ))),
        1 => {
            let m = &matches[0];
            Ok(EditSpan {
                start: m.start(),
                end: m.end(),
            })
        }
        n => Err(EditError::AmbiguousMatch {
            count: n,
            offsets: matches.iter().map(|m| m.start()).collect(),
        }),
    }
}

/// Convert a byte offset in a string to a Unicode code point offset.
///
/// The Automerge document uses `TextEncoding::UnicodeCodePoint`, so splice
/// indices must be code-point counts, not byte offsets.
pub fn byte_offset_to_codepoint(s: &str, byte_offset: usize) -> usize {
    s[..byte_offset].chars().count()
}

/// Apply a replacement at the given span.
pub fn apply_replacement(source: &str, span: &EditSpan, replacement: &str) -> String {
    let mut result = String::with_capacity(source.len() + replacement.len());
    result.push_str(&source[..span.start]);
    result.push_str(replacement);
    result.push_str(&source[span.end..]);
    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    // ── resolve_match: no context ───────────────────────────────

    #[test]
    fn match_unique_no_context() {
        let source = "hello world";
        let span = resolve_match(source, "world", None, None).unwrap();
        assert_eq!(&source[span.start..span.end], "world");
    }

    #[test]
    fn match_ambiguous_no_context() {
        let source = "bar bar bar";
        let err = resolve_match(source, "bar", None, None).unwrap_err();
        assert!(matches!(err, EditError::AmbiguousMatch { count: 3, .. }));
    }

    #[test]
    fn match_not_found() {
        let source = "hello world";
        let err = resolve_match(source, "xyz", None, None).unwrap_err();
        assert!(matches!(err, EditError::NoMatch(_)));
    }

    // ── resolve_match: context_before ───────────────────────────

    #[test]
    fn context_before_disambiguates() {
        let source = "a = \"foo bar\"\nb = \"baz bar\"\nc = \"qux bar\"";
        let span = resolve_match(source, "bar", Some("baz"), None).unwrap();
        let replaced = apply_replacement(source, &span, "BAR");
        assert_eq!(
            replaced,
            "a = \"foo bar\"\nb = \"baz BAR\"\nc = \"qux bar\""
        );
    }

    #[test]
    fn context_before_with_gap() {
        // "goodbye" is before "world" but not immediately adjacent
        let source = "greeting = \"hello nteract\"\nfarewell = \"goodbye world\"";
        let span = resolve_match(source, "world", Some("goodbye"), None).unwrap();
        assert_eq!(&source[span.start..span.end], "world");
    }

    #[test]
    fn context_before_no_match() {
        let source = "a = \"foo bar\"\nb = \"baz bar\"";
        let err = resolve_match(source, "bar", Some("zzz"), None).unwrap_err();
        assert!(matches!(err, EditError::NoMatch(_)));
    }

    // ── resolve_match: context_after ────────────────────────────

    #[test]
    fn context_after_disambiguates() {
        let source = "a = \"foo bar\"\nb = \"baz bar\"\nc = \"qux bar\"";
        let span = resolve_match(source, "bar", None, Some("c =")).unwrap();
        let replaced = apply_replacement(source, &span, "BAR");
        assert_eq!(
            replaced,
            "a = \"foo bar\"\nb = \"baz BAR\"\nc = \"qux bar\""
        );
    }

    // ── resolve_match: both contexts ────────────────────────────

    #[test]
    fn both_contexts() {
        let source = "x bar y\na bar b\nx bar y";
        let span = resolve_match(source, "bar", Some("a"), Some("b")).unwrap();
        let replaced = apply_replacement(source, &span, "BAR");
        assert_eq!(replaced, "x bar y\na BAR b\nx bar y");
    }

    // ── resolve_match: unique match with context still works ────

    #[test]
    fn unique_match_with_context_succeeds() {
        let source = "greeting = \"hello nteract\"\nfarewell = \"goodbye world\"\nprint(greeting)\nprint(farewell)";
        let span = resolve_match(source, "world", Some("goodbye"), None).unwrap();
        let replaced = apply_replacement(source, &span, "universe");
        assert!(replaced.contains("goodbye universe"));
    }

    // ── apply_replacement ───────────────────────────────────────

    #[test]
    fn apply_basic_replacement() {
        let source = "hello world";
        let span = EditSpan { start: 6, end: 11 };
        assert_eq!(apply_replacement(source, &span, "rust"), "hello rust");
    }

    // ── resolve_regex ───────────────────────────────────────────

    #[test]
    fn regex_unique() {
        let source = "foo = 42\nbar = 99";
        let span = resolve_regex(source, r"\d+").unwrap_err();
        // Two matches → ambiguous
        assert!(matches!(span, EditError::AmbiguousMatch { count: 2, .. }));
    }

    #[test]
    fn regex_multiline_anchor() {
        let source = "foo = 42\nbar = 99";
        let span = resolve_regex(source, r"^bar = \d+$").unwrap();
        assert_eq!(&source[span.start..span.end], "bar = 99");
    }
}
