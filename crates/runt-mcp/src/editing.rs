//! Pattern resolution for replace_match and replace_regex tools.
//!
//! Ports the Python `_editing.py` logic to Rust. The key contract:
//! - `replace_match`: exact literal match with optional context disambiguation
//! - `replace_regex`: Python-compatible regex with MULTILINE flag
//!
//! Both require exactly one match to succeed.

/// An edit span in the source text (byte offsets).
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

    // Build a regex that includes optional context
    let escaped_match = regex::escape(match_text);
    let pattern = match (context_before, context_after) {
        (Some(before), Some(after)) => {
            format!(
                "{}{}{}",
                regex::escape(before),
                escaped_match,
                regex::escape(after)
            )
        }
        (Some(before), None) => format!("{}{}", regex::escape(before), escaped_match),
        (None, Some(after)) => format!("{}{}", escaped_match, regex::escape(after)),
        (None, None) => escaped_match,
    };

    let re = regex::Regex::new(&pattern).map_err(|e| EditError::InvalidPattern(e.to_string()))?;
    let matches: Vec<regex::Match> = re.find_iter(source).collect();

    match matches.len() {
        0 => Err(EditError::NoMatch(format!(
            "No match found for '{match_text}'"
        ))),
        1 => {
            let m = &matches[0];
            // Calculate the offset of the actual match within the full pattern
            let context_before_len = context_before.map(|s| s.len()).unwrap_or(0);
            let match_start = m.start() + context_before_len;
            let match_end = match_start + match_text.len();
            Ok(EditSpan {
                start: match_start,
                end: match_end,
            })
        }
        n => Err(EditError::AmbiguousMatch {
            count: n,
            offsets: matches.iter().map(|m| m.start()).collect(),
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
