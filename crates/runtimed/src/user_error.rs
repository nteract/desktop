//! `UserErrorOutput` — the canonical "user code raised" event.
//!
//! IOPub can represent a user error two ways:
//!
//! 1. **Classic.** `JupyterMessageContent::ErrorOutput` carrying
//!    `{ename, evalue, traceback[]}` where the traceback entries are
//!    ANSI-coded strings. What vanilla IPython emits.
//!
//! 2. **Rich.** `JupyterMessageContent::DisplayData` with our custom
//!    MIME `application/vnd.nteract.traceback+json` and a structured
//!    payload (ename/evalue/frames with source-context windows and
//!    highlight markers). What `nteract-kernel-launcher` emits after
//!    hooking `_showtraceback`.
//!
//! Downstream code shouldn't care which wire shape arrived. The daemon
//! needs to:
//!
//! - mark the execution as errored (runtime state, queue, success flag),
//! - persist a nbformat-compliant `output_type: "error"` on `.ipynb`,
//! - give the frontend the richest renderer it can.
//!
//! `UserErrorOutput` is the single semantic type every site routes
//! through. Classifying at the IOPub / nbformat edges means the rest
//! of the system stays flat — no ad-hoc "is this MIME special?"
//! sniffing sprinkled across dispatchers.

use serde::{Deserialize, Serialize};

/// The MIME used by the launcher's rich traceback payload. Shared with
/// the frontend renderer. Single source of truth.
pub const TRACEBACK_MIME: &str = "application/vnd.nteract.traceback+json";

/// One source-context line within a rich frame.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RichLine {
    pub lineno: u32,
    pub source: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub highlight: bool,
}

/// A single frame in a rich traceback.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RichFrame {
    pub filename: String,
    pub lineno: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<Vec<RichLine>>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub library: bool,
}

/// Parse-error-only slot populated for `SyntaxError` /
/// `IndentationError` / `TabError`. Carries the caret info the
/// exception object exposes (`offset`, `text`, `msg`, and 3.11+
/// `end_offset`/`end_lineno` for range underline) so the renderer
/// can show the offending source line instead of a useless frame list.
///
/// `end_lineno` / `end_offset` of 0 means "absent" (the emitter
/// normalizes CPython's `-1` sentinel to 0).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RichSyntax {
    pub filename: String,
    pub lineno: u32,
    pub offset: u32,
    #[serde(default)]
    pub end_lineno: u32,
    #[serde(default)]
    pub end_offset: u32,
    pub text: String,
    pub msg: String,
}

/// The rich payload shape the frontend's `TracebackOutput` consumes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RichTraceback {
    pub ename: String,
    pub evalue: String,
    pub frames: Vec<RichFrame>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Paste-ready plain text — the ANSI-stripped `traceback.format_exception`
    /// output. Frontend Copy button writes this verbatim.
    pub text: String,
    /// Present for parse errors. When set, the renderer shows a
    /// dedicated source-line + caret layout instead of a frame list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub syntax: Option<RichSyntax>,
}

/// A user-code error, regardless of wire shape.
#[derive(Debug, Clone)]
pub enum UserErrorOutput {
    /// Classic Jupyter `ErrorOutput`: ANSI traceback strings.
    Classic {
        ename: String,
        evalue: String,
        /// Traceback lines. Usually ANSI-coded; may be empty.
        traceback: Vec<String>,
    },
    /// Launcher-emitted rich payload.
    Rich(RichTraceback),
}

impl UserErrorOutput {
    /// Read an IOPub `JupyterMessageContent` and recognize a user error.
    ///
    /// Returns:
    /// - `Classic` for `ErrorOutput { ename, evalue, traceback }`
    /// - `Rich` for `DisplayData` / `ExecuteResult` whose `data` contains
    ///   [`TRACEBACK_MIME`]
    /// - `None` otherwise
    ///
    /// This is the runtime agent's single entry point for deciding
    /// "did user code raise?" — both wire shapes route through it.
    pub fn from_iopub(content: &jupyter_protocol::JupyterMessageContent) -> Option<Self> {
        use jupyter_protocol::JupyterMessageContent as J;
        match content {
            J::ErrorOutput(err) => Some(UserErrorOutput::Classic {
                ename: err.ename.clone(),
                evalue: err.evalue.clone(),
                traceback: err.traceback.clone(),
            }),
            J::DisplayData(dd) => media_rich(&dd.data).map(UserErrorOutput::Rich),
            J::ExecuteResult(er) => media_rich(&er.data).map(UserErrorOutput::Rich),
            _ => None,
        }
    }

    /// Read an nbformat-shaped output value and recognize a user error.
    ///
    /// Returns:
    /// - `Classic` for `{"output_type": "error", …}`
    /// - `Rich`   for `{"output_type": "display_data", "data": {TRACEBACK_MIME: …}}`
    /// - `None`   otherwise
    ///
    /// The rich payload may arrive either as a JSON object (preferred)
    /// or a stringified JSON (some IOPub paths stringify vnd.* MIMEs).
    /// Both are accepted.
    pub fn from_nbformat(output: &serde_json::Value) -> Option<Self> {
        let output_type = output.get("output_type")?.as_str()?;
        match output_type {
            "error" => {
                let ename = output.get("ename")?.as_str()?.to_string();
                let evalue = output.get("evalue")?.as_str()?.to_string();
                let traceback = output
                    .get("traceback")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                Some(UserErrorOutput::Classic {
                    ename,
                    evalue,
                    traceback,
                })
            }
            "display_data" | "execute_result" => {
                let raw = output.get("data")?.get(TRACEBACK_MIME)?;
                rich_from_value(raw).map(UserErrorOutput::Rich)
            }
            _ => None,
        }
    }

    /// Project to the nbformat-compliant classic shape, always.
    /// Used on `.ipynb` save so on-disk files stay standards-correct.
    pub fn to_classic(&self) -> (String, String, Vec<String>) {
        match self {
            UserErrorOutput::Classic {
                ename,
                evalue,
                traceback,
            } => (ename.clone(), evalue.clone(), traceback.clone()),
            UserErrorOutput::Rich(rt) => {
                // `rt.text` is `traceback.format_exception` joined — the
                // canonical `.ipynb` traceback form. Split on newlines so
                // it round-trips through the string-array nbformat slot.
                let tb: Vec<String> = if rt.text.is_empty() {
                    Vec::new()
                } else {
                    rt.text
                        .split_inclusive('\n')
                        .map(|s| s.trim_end_matches('\n').to_string())
                        .collect()
                };
                (rt.ename.clone(), rt.evalue.clone(), tb)
            }
        }
    }

    /// The rich form, synthesizing from the classic shape when needed.
    ///
    /// Returns `None` only when we have neither a rich payload nor enough
    /// to build one (no frames parsed out of the ANSI strings).
    pub fn to_rich(&self) -> Option<RichTraceback> {
        match self {
            UserErrorOutput::Rich(rt) => Some(rt.clone()),
            UserErrorOutput::Classic {
                ename,
                evalue,
                traceback,
            } => parse_ansi_traceback(ename, evalue, traceback),
        }
    }
}

// ─── Rich payload coercion ────────────────────────────────────────────────

fn rich_from_value(raw: &serde_json::Value) -> Option<RichTraceback> {
    match raw {
        serde_json::Value::String(s) => serde_json::from_str(s).ok(),
        other => serde_json::from_value(other.clone()).ok(),
    }
}

/// Scan a `Media` bundle for our traceback MIME and decode the payload.
///
/// The `MediaType::Other((mime, value))` arm is where custom MIMEs land
/// on the Rust side after jupyter-protocol deserializes IOPub JSON.
/// We accept both object and stringified-JSON payload shapes (mirrors
/// the nbformat path — some publish routes stringify vnd.* MIMEs).
fn media_rich(data: &jupyter_protocol::Media) -> Option<RichTraceback> {
    for mt in &data.content {
        if let jupyter_protocol::MediaType::Other((mime, value)) = mt {
            if mime == TRACEBACK_MIME {
                return rich_from_value(value);
            }
        }
    }
    None
}

// ─── ANSI traceback parser ────────────────────────────────────────────────

/// Best-effort parse of IPython's ANSI traceback strings into
/// [`RichTraceback`]. Returns `None` when no frame could be recovered —
/// the caller should fall back to the classic-only render rather than
/// show a header-only panel.
pub fn parse_ansi_traceback(
    ename: &str,
    evalue: &str,
    traceback: &[String],
) -> Option<RichTraceback> {
    let joined = traceback.join("\n");
    if joined.is_empty() && ename.is_empty() && evalue.is_empty() {
        return None;
    }
    let stripped = strip_ansi(&joined);
    let (frames, last_exception) = scan_frames(&stripped);
    if frames.is_empty() {
        return None;
    }
    let effective_ename = if ename.is_empty() {
        last_exception
            .as_ref()
            .map(|(e, _)| e.clone())
            .unwrap_or_else(|| "Error".to_string())
    } else {
        ename.to_string()
    };
    let effective_evalue = if evalue.is_empty() {
        last_exception
            .as_ref()
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    } else {
        evalue.to_string()
    };
    let text = build_paste_text(&stripped, &effective_ename, &effective_evalue);
    Some(RichTraceback {
        ename: effective_ename,
        evalue: effective_evalue,
        frames,
        language: Some("python".to_string()),
        text,
        syntax: None,
    })
}

/// Strip CSI / OSC ANSI escape sequences. Operates on UTF-8 via chars.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek() {
                Some(&'[') => {
                    chars.next();
                    for cc in chars.by_ref() {
                        let code = cc as u32;
                        if (0x40..=0x7e).contains(&code) {
                            break;
                        }
                    }
                }
                Some(&']') => {
                    chars.next();
                    while let Some(cc) = chars.next() {
                        if cc == '\u{07}' {
                            break;
                        }
                        if cc == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn is_library_frame(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    lower.contains("site-packages")
        || lower.contains("dist-packages")
        || lower.contains("/lib/python")
        || lower.contains("\\lib\\python")
        || lower.contains("python.framework")
}

struct PendingFrame {
    filename: String,
    lineno: u32,
    name: String,
    lines: Vec<RichLine>,
}

impl PendingFrame {
    fn finalize(self) -> RichFrame {
        let library = is_library_frame(&self.filename);
        RichFrame {
            filename: self.filename,
            lineno: self.lineno,
            name: self.name,
            lines: if self.lines.is_empty() {
                None
            } else {
                Some(self.lines)
            },
            library,
        }
    }
}

type ExceptionLine = (String, String);

fn scan_frames(stripped: &str) -> (Vec<RichFrame>, Option<ExceptionLine>) {
    let mut frames: Vec<RichFrame> = Vec::new();
    let mut cur: Option<PendingFrame> = None;
    let mut last_exception: Option<ExceptionLine> = None;

    for line in stripped.lines() {
        if let Some((file, lineno, name)) = match_file_frame(line) {
            if let Some(p) = cur.take() {
                frames.push(p.finalize());
            }
            cur = Some(PendingFrame {
                filename: file,
                lineno,
                name,
                lines: Vec::new(),
            });
            continue;
        }
        if let Some((idx, lineno, name)) = match_cell_frame(line) {
            if let Some(p) = cur.take() {
                frames.push(p.finalize());
            }
            cur = Some(PendingFrame {
                filename: format!("Cell In[{idx}]"),
                lineno,
                name,
                lines: Vec::new(),
            });
            continue;
        }
        if let Some((idx, name)) = match_input_frame(line) {
            if let Some(p) = cur.take() {
                frames.push(p.finalize());
            }
            cur = Some(PendingFrame {
                filename: format!("<ipython-input-{idx}>"),
                lineno: 0,
                name,
                lines: Vec::new(),
            });
            continue;
        }

        // Exception lines can appear inside or outside an open frame
        // (they're the tail after the last frame's source), so check
        // them unconditionally. Latest wins — that's the terminating
        // exception whose ename/evalue we want.
        if let Some((class, msg)) = match_exception_line(line) {
            last_exception = Some((class, msg));
            continue;
        }

        if let Some(frame) = cur.as_mut() {
            if is_caret_rail(line) {
                continue;
            }
            if let Some((highlight, lineno, source)) = match_numbered_source(line) {
                frame.lines.push(RichLine {
                    lineno,
                    source,
                    highlight,
                });
                continue;
            }
            // Non-numbered content inside a frame — dropped to stay loose.
        }
    }
    if let Some(p) = cur.take() {
        frames.push(p.finalize());
    }
    (frames, last_exception)
}

fn take_number(s: &str) -> (&str, &str) {
    let end = s
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    s.split_at(end)
}

fn match_file_frame(line: &str) -> Option<(String, u32, String)> {
    let t = line.trim_start();
    let rest = t.strip_prefix("File ")?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('"')?;
    let close = rest.find('"')?;
    let filename = rest[..close].to_string();
    let rest = &rest[close + 1..];
    let rest = rest.strip_prefix(',')?.trim_start();
    let rest = rest.strip_prefix("line ")?;
    let (num, after) = take_number(rest);
    let lineno: u32 = num.parse().ok()?;
    let name = after
        .trim_start_matches(|c: char| c == ',' || c.is_whitespace())
        .strip_prefix("in ")
        .map(|n| n.trim().to_string())
        .unwrap_or_else(|| "<module>".to_string());
    Some((filename, lineno, name))
}

fn match_cell_frame(line: &str) -> Option<(u32, u32, String)> {
    let t = line.trim_start();
    let rest = t.strip_prefix("Cell In[")?;
    let end = rest.find(']')?;
    let idx: u32 = rest[..end].parse().ok()?;
    let rest = rest[end + 1..].trim_start();
    let rest = rest.strip_prefix(',')?.trim_start();
    let rest = rest.strip_prefix("line ")?;
    let (num, after) = take_number(rest);
    let lineno: u32 = num.parse().ok()?;
    let name = after
        .trim_start_matches(|c: char| c == ',' || c.is_whitespace())
        .strip_prefix("in ")
        .map(|n| n.trim().to_string())
        .unwrap_or_else(|| "<cell>".to_string());
    Some((idx, lineno, name))
}

fn match_input_frame(line: &str) -> Option<(u32, String)> {
    let t = line.trim_start();
    let rest = t.strip_prefix("<ipython-input-")?;
    let (num, after) = take_number(rest);
    let idx: u32 = num.parse().ok()?;
    let rest = after.trim_start_matches(|c: char| c != '>');
    let rest = rest.strip_prefix('>')?.trim();
    let name = rest.strip_prefix("in ")?.trim().to_string();
    Some((idx, name))
}

fn match_numbered_source(line: &str) -> Option<(bool, u32, String)> {
    let t = line.trim_end();
    let trimmed = t.trim_start();
    let (highlight, rest) = if let Some(s) = trimmed
        .strip_prefix("---->")
        .or_else(|| trimmed.strip_prefix("--->"))
        .or_else(|| trimmed.strip_prefix("-->"))
        .or_else(|| trimmed.strip_prefix("->"))
        .or_else(|| trimmed.strip_prefix('→'))
        .or_else(|| trimmed.strip_prefix('▸'))
    {
        (true, s.trim_start())
    } else {
        (false, trimmed)
    };
    let (num, after) = take_number(rest);
    if num.is_empty() {
        return None;
    }
    let lineno: u32 = num.parse().ok()?;
    let source = after
        .strip_prefix(|c: char| c == ' ' || c == '\t')
        .unwrap_or(after)
        .to_string();
    Some((highlight, lineno, source))
}

fn is_caret_rail(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && t.chars().all(|c| c == '^' || c == '~')
}

fn match_exception_line(line: &str) -> Option<ExceptionLine> {
    let t = line.trim_start();
    let idx = t.find(": ")?;
    let (class, rest) = t.split_at(idx);
    if class.is_empty() {
        return None;
    }
    if !class
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return None;
    }
    if !(class.ends_with("Error")
        || class.ends_with("Warning")
        || class.ends_with("Exception")
        || class.ends_with("Interrupt")
        || class.ends_with("Exit"))
    {
        return None;
    }
    let msg = rest.trim_start_matches(": ").to_string();
    Some((class.to_string(), msg))
}

fn build_paste_text(stripped: &str, ename: &str, evalue: &str) -> String {
    let tail = format!("{ename}: {evalue}");
    if stripped.trim_end().ends_with(&tail) {
        stripped.to_string()
    } else {
        let mut s = stripped.trim_end().to_string();
        s.push('\n');
        s.push_str(&tail);
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── UserErrorOutput::from_iopub ──────────────────────────────────

    fn rich_payload() -> serde_json::Value {
        json!({
            "ename": "KeyError",
            "evalue": "'x'",
            "frames": [{"filename": "/tmp/x.py", "lineno": 1, "name": "f"}],
            "text": "KeyError: 'x'",
        })
    }

    #[test]
    fn from_iopub_error_output_is_classic() {
        let content =
            jupyter_protocol::JupyterMessageContent::ErrorOutput(jupyter_protocol::ErrorOutput {
                ename: "ZeroDivisionError".into(),
                evalue: "division by zero".into(),
                traceback: vec!["line1".into(), "line2".into()],
            });
        let ue = UserErrorOutput::from_iopub(&content).unwrap();
        match ue {
            UserErrorOutput::Classic {
                ename,
                evalue,
                traceback,
            } => {
                assert_eq!(ename, "ZeroDivisionError");
                assert_eq!(evalue, "division by zero");
                assert_eq!(traceback.len(), 2);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn from_iopub_display_data_with_mime_is_rich() {
        let data = jupyter_protocol::Media {
            content: vec![jupyter_protocol::MediaType::Other((
                TRACEBACK_MIME.to_string(),
                rich_payload(),
            ))],
        };
        let content =
            jupyter_protocol::JupyterMessageContent::DisplayData(jupyter_protocol::DisplayData {
                data,
                metadata: Default::default(),
                transient: None,
            });
        let ue = UserErrorOutput::from_iopub(&content).unwrap();
        match ue {
            UserErrorOutput::Rich(rt) => {
                assert_eq!(rt.ename, "KeyError");
                assert_eq!(rt.evalue, "'x'");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn from_iopub_execute_result_with_mime_is_rich() {
        // Some emitters use execute_result for last-expression errors.
        let data = jupyter_protocol::Media {
            content: vec![jupyter_protocol::MediaType::Other((
                TRACEBACK_MIME.to_string(),
                rich_payload(),
            ))],
        };
        let content = jupyter_protocol::JupyterMessageContent::ExecuteResult(
            jupyter_protocol::ExecuteResult {
                execution_count: jupyter_protocol::ExecutionCount::new(1),
                data,
                metadata: Default::default(),
                transient: None,
            },
        );
        let ue = UserErrorOutput::from_iopub(&content).unwrap();
        assert!(matches!(ue, UserErrorOutput::Rich(_)));
    }

    #[test]
    fn from_iopub_display_data_without_mime_is_none() {
        let data = jupyter_protocol::Media {
            content: vec![jupyter_protocol::MediaType::Plain("plain text".into())],
        };
        let content =
            jupyter_protocol::JupyterMessageContent::DisplayData(jupyter_protocol::DisplayData {
                data,
                metadata: Default::default(),
                transient: None,
            });
        assert!(UserErrorOutput::from_iopub(&content).is_none());
    }

    #[test]
    fn from_iopub_stringified_mime_payload_is_rich() {
        // Mirror the nbformat stringified case: the value can be a JSON string.
        let as_str = serde_json::to_string(&rich_payload()).unwrap();
        let data = jupyter_protocol::Media {
            content: vec![jupyter_protocol::MediaType::Other((
                TRACEBACK_MIME.to_string(),
                serde_json::Value::String(as_str),
            ))],
        };
        let content =
            jupyter_protocol::JupyterMessageContent::DisplayData(jupyter_protocol::DisplayData {
                data,
                metadata: Default::default(),
                transient: None,
            });
        assert!(matches!(
            UserErrorOutput::from_iopub(&content).unwrap(),
            UserErrorOutput::Rich(_)
        ));
    }

    #[test]
    fn from_iopub_unrelated_messages_are_none() {
        let content = jupyter_protocol::JupyterMessageContent::StreamContent(
            jupyter_protocol::StreamContent {
                name: jupyter_protocol::Stdio::Stdout,
                text: "hi\n".into(),
            },
        );
        assert!(UserErrorOutput::from_iopub(&content).is_none());
    }

    // ── UserErrorOutput::from_nbformat ───────────────────────────────

    #[test]
    fn from_nbformat_classic_error() {
        let v = json!({
            "output_type": "error",
            "ename": "ZeroDivisionError",
            "evalue": "division by zero",
            "traceback": ["Traceback", "  File \"/tmp/x.py\", line 1", "ZeroDivisionError: division by zero"],
        });
        let ue = UserErrorOutput::from_nbformat(&v).unwrap();
        match ue {
            UserErrorOutput::Classic {
                ename,
                evalue,
                traceback,
            } => {
                assert_eq!(ename, "ZeroDivisionError");
                assert_eq!(evalue, "division by zero");
                assert_eq!(traceback.len(), 3);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn from_nbformat_rich_display_data_object() {
        let payload = json!({
            "ename": "KeyError",
            "evalue": "'missing'",
            "frames": [{
                "filename": "/tmp/x.py",
                "lineno": 2,
                "name": "f",
                "library": false,
            }],
            "text": "KeyError: 'missing'",
        });
        let v = json!({
            "output_type": "display_data",
            "data": { TRACEBACK_MIME: payload },
        });
        let ue = UserErrorOutput::from_nbformat(&v).unwrap();
        match ue {
            UserErrorOutput::Rich(rt) => {
                assert_eq!(rt.ename, "KeyError");
                assert_eq!(rt.frames.len(), 1);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn from_nbformat_rich_display_data_json_string() {
        // Some IOPub paths stringify vnd.* MIMEs; accept either shape.
        let payload = serde_json::to_string(&json!({
            "ename": "ValueError",
            "evalue": "bad",
            "frames": [{"filename": "/tmp/x.py", "lineno": 1, "name": "f"}],
            "text": "ValueError: bad",
        }))
        .unwrap();
        let v = json!({
            "output_type": "display_data",
            "data": { TRACEBACK_MIME: payload },
        });
        let ue = UserErrorOutput::from_nbformat(&v).unwrap();
        matches!(ue, UserErrorOutput::Rich(_));
    }

    #[test]
    fn from_nbformat_none_for_unrelated() {
        assert!(UserErrorOutput::from_nbformat(
            &json!({"output_type": "stream", "name": "stdout", "text": "hi"})
        )
        .is_none());
        assert!(UserErrorOutput::from_nbformat(
            &json!({"output_type": "display_data", "data": {"text/plain": "x"}})
        )
        .is_none());
    }

    // ── to_classic ──────────────────────────────────────────────────

    #[test]
    fn rich_to_classic_splits_text_on_newlines() {
        let rt = RichTraceback {
            ename: "ZeroDivisionError".into(),
            evalue: "division by zero".into(),
            frames: vec![],
            language: Some("python".into()),
            text: "Traceback (most recent call last):\n  File \"/tmp/x.py\", line 1, in <module>\n    1/0\nZeroDivisionError: division by zero".into(),
            syntax: None,
        };
        let ue = UserErrorOutput::Rich(rt);
        let (ename, evalue, tb) = ue.to_classic();
        assert_eq!(ename, "ZeroDivisionError");
        assert_eq!(evalue, "division by zero");
        assert_eq!(tb.len(), 4);
        assert!(tb[0].starts_with("Traceback"));
        assert!(tb[3].starts_with("ZeroDivisionError"));
    }

    #[test]
    fn classic_to_classic_is_identity() {
        let ue = UserErrorOutput::Classic {
            ename: "E".into(),
            evalue: "v".into(),
            traceback: vec!["line1".into(), "line2".into()],
        };
        let (ename, evalue, tb) = ue.to_classic();
        assert_eq!(ename, "E");
        assert_eq!(evalue, "v");
        assert_eq!(tb, vec!["line1".to_string(), "line2".to_string()]);
    }

    // ── ANSI parser ─────────────────────────────────────────────────

    #[test]
    fn parses_cpython_frame() {
        let tb = vec![
            "Traceback (most recent call last):".to_string(),
            "  File \"/tmp/foo.py\", line 10, in main".to_string(),
            "    return divide(1, 0)".to_string(),
            "  File \"/tmp/foo.py\", line 4, in divide".to_string(),
            "    return a / b".to_string(),
            "ZeroDivisionError: division by zero".to_string(),
        ];
        let rt = parse_ansi_traceback("ZeroDivisionError", "division by zero", &tb).unwrap();
        assert_eq!(rt.frames.len(), 2);
        assert_eq!(rt.frames[0].name, "main");
        assert_eq!(rt.frames[1].name, "divide");
    }

    #[test]
    fn parses_jupyter_cell() {
        let tb = vec![
            "Cell In[3], line 2, in <module>".to_string(),
            "     1 try:".to_string(),
            "---> 2     outer()".to_string(),
        ];
        let rt = parse_ansi_traceback("ValueError", "nope", &tb).unwrap();
        assert_eq!(rt.frames[0].filename, "Cell In[3]");
        let lines = rt.frames[0].lines.as_ref().unwrap();
        assert!(lines.iter().any(|l| l.highlight));
    }

    #[test]
    fn strips_ansi_escapes() {
        let tb = vec![
            "\x1b[0;36m  File \x1b[0m\"\x1b[0;32m/tmp/foo.py\x1b[0m\", line \x1b[0;36m9\x1b[0m, in \x1b[0;36mhelper\x1b[0m".to_string(),
        ];
        let rt = parse_ansi_traceback("E", "v", &tb).unwrap();
        assert_eq!(rt.frames[0].filename, "/tmp/foo.py");
        assert_eq!(rt.frames[0].name, "helper");
    }

    #[test]
    fn flags_library_frames() {
        let tb = vec![
            "  File \"/opt/python3.13/site-packages/foo/bar.py\", line 9, in helper".to_string(),
            "    raise RuntimeError('boom')".to_string(),
        ];
        let rt = parse_ansi_traceback("RuntimeError", "boom", &tb).unwrap();
        assert!(rt.frames[0].library);
    }

    #[test]
    fn returns_none_without_frames() {
        // No File/Cell/input headers → refuse to synthesize.
        let tb = vec!["ValueError: oops".to_string()];
        assert!(parse_ansi_traceback("ValueError", "oops", &tb).is_none());
    }

    #[test]
    fn text_ends_with_class_colon_message() {
        let tb = vec![
            "  File \"/tmp/x.py\", line 1, in <module>".to_string(),
            "    x = 1/0".to_string(),
            "ZeroDivisionError: division by zero".to_string(),
        ];
        let rt = parse_ansi_traceback("", "", &tb).unwrap();
        assert_eq!(rt.ename, "ZeroDivisionError");
        assert_eq!(rt.evalue, "division by zero");
        assert!(rt
            .text
            .trim_end()
            .ends_with("ZeroDivisionError: division by zero"));
    }

    #[test]
    fn to_rich_for_classic_parses_ansi() {
        let ue = UserErrorOutput::Classic {
            ename: "ZeroDivisionError".into(),
            evalue: "division by zero".into(),
            traceback: vec![
                "  File \"/tmp/x.py\", line 1, in <module>".to_string(),
                "    1/0".to_string(),
            ],
        };
        let rt = ue.to_rich().unwrap();
        assert_eq!(rt.frames.len(), 1);
    }

    // ── Coverage gaps Codex flagged: document behavior explicitly ──

    #[test]
    fn chained_raise_from_parses_flat_with_separator_in_text() {
        // `raise X() from Y()` produces two traceback segments joined by
        // "The above exception was the direct cause of the following
        // exception:". Our parser treats them flat — all File frames get
        // captured, the last exception line wins, and the separator text
        // survives in `rt.text` via build_paste_text.
        let tb = vec![
            "Traceback (most recent call last):".to_string(),
            "  File \"/tmp/a.py\", line 1, in root_cause".to_string(),
            "    raise ValueError(\"cause\")".to_string(),
            "ValueError: cause".to_string(),
            "".to_string(),
            "The above exception was the direct cause of the following exception:".to_string(),
            "".to_string(),
            "Traceback (most recent call last):".to_string(),
            "  File \"/tmp/b.py\", line 4, in outer".to_string(),
            "    raise RuntimeError(\"wrapper\") from exc".to_string(),
            "RuntimeError: wrapper".to_string(),
        ];
        let rt = parse_ansi_traceback("RuntimeError", "wrapper", &tb).unwrap();
        assert_eq!(rt.frames.len(), 2);
        assert_eq!(rt.frames[0].name, "root_cause");
        assert_eq!(rt.frames[1].name, "outer");
        // Outer exception (the one raised last) is what the user sees.
        assert_eq!(rt.ename, "RuntimeError");
        assert_eq!(rt.evalue, "wrapper");
        // Separator preserved in the copy-ready text.
        assert!(rt
            .text
            .contains("The above exception was the direct cause of the following exception:"));
    }

    #[test]
    fn during_handling_separator_preserved_in_text() {
        let tb = vec![
            "Traceback (most recent call last):".to_string(),
            "  File \"/tmp/a.py\", line 1, in first".to_string(),
            "    raise ValueError(\"first\")".to_string(),
            "ValueError: first".to_string(),
            "".to_string(),
            "During handling of the above exception, another exception occurred:".to_string(),
            "".to_string(),
            "Traceback (most recent call last):".to_string(),
            "  File \"/tmp/b.py\", line 2, in second".to_string(),
            "    raise RuntimeError(\"second\")".to_string(),
            "RuntimeError: second".to_string(),
        ];
        let rt = parse_ansi_traceback("RuntimeError", "second", &tb).unwrap();
        assert_eq!(rt.frames.len(), 2);
        assert!(rt
            .text
            .contains("During handling of the above exception, another exception occurred:"));
    }

    #[test]
    fn exception_group_falls_back_to_none_today() {
        // Python 3.11 ExceptionGroup tracebacks prefix nested frames with
        // `|`. Our parser doesn't recognize `|   File ...`, and
        // `ExceptionGroup: ...` doesn't match the exception-line allowlist
        // (`ExceptionGroup` doesn't end with Error/Warning/Exception/...).
        // Documenting: today, ExceptionGroup-only tracebacks yield `None`,
        // and the caller falls back to the Classic ANSI render. Revisit
        // if/when we decide to natively support nested groups.
        let tb = vec![
            "  + Exception Group Traceback (most recent call last):".to_string(),
            "  |   File \"/tmp/x.py\", line 1, in <module>".to_string(),
            "  |     raise ExceptionGroup(\"wrap\", [ValueError(\"a\")])".to_string(),
            "  | ExceptionGroup: wrap (1 sub-exception)".to_string(),
            "  +-+---------------- 1 ----------------".to_string(),
            "    | ValueError: a".to_string(),
            "    +------------------------------------".to_string(),
        ];
        // ename/evalue not provided — we can't recover anything useful.
        assert!(parse_ansi_traceback("", "", &tb).is_none());
    }

    #[test]
    fn multiline_evalue_roundtrip_via_rich_to_classic() {
        // Some Python exceptions carry embedded newlines in their message
        // (e.g. AssertionError in pytest, SQLAlchemy chained contexts).
        // A Rich variant's `to_classic()` must preserve them in the
        // resulting traceback-array entries.
        let rt = RichTraceback {
            ename: "AssertionError".into(),
            evalue: "line one\nline two\nline three".into(),
            frames: vec![],
            language: Some("python".into()),
            text: "Traceback (most recent call last):\n  File \"/tmp/x.py\", line 1, in t\n    assert False, \"line one\\nline two\\nline three\"\nAssertionError: line one\nline two\nline three".into(),
            syntax: None,
        };
        let (ename, evalue, tb) = UserErrorOutput::Rich(rt).to_classic();
        assert_eq!(ename, "AssertionError");
        assert!(evalue.contains("line one") && evalue.contains("line three"));
        // Interior blank lines would become empty strings in the array;
        // here we just confirm the line count grew with the multi-line
        // tail.
        assert!(tb.len() >= 6);
    }

    #[test]
    fn to_classic_drops_final_trailing_newline_only() {
        // traceback.format_exception typically ends with a single
        // trailing "\n". split_inclusive keeps the newline ON each
        // entry; trim_end_matches('\n') strips exactly one trailing
        // newline per line. The final empty-after-the-newline element
        // split_inclusive produces for a trailing '\n' gets trimmed to
        // empty string, which is an acceptable (empty) array entry.
        let rt = RichTraceback {
            ename: "E".into(),
            evalue: "v".into(),
            frames: vec![],
            language: Some("python".into()),
            text: "line1\nline2\n".into(),
            syntax: None,
        };
        let (_, _, tb) = UserErrorOutput::Rich(rt).to_classic();
        // "line1\n" "line2\n" → ["line1", "line2"].
        // `split_inclusive` does NOT produce a trailing empty entry for
        // "foo\n", so we get exactly two strings.
        assert_eq!(tb, vec!["line1".to_string(), "line2".to_string()]);
    }

    #[test]
    fn from_nbformat_accepts_execute_result_rich_candidate() {
        // `execute_result` with our MIME should also classify as Rich
        // (some emitters use execute_result for the last-expression shape).
        let payload = json!({
            "ename": "KeyError",
            "evalue": "'x'",
            "frames": [{"filename": "/tmp/x.py", "lineno": 1, "name": "f"}],
            "text": "KeyError: 'x'",
        });
        let v = json!({
            "output_type": "execute_result",
            "execution_count": 1,
            "data": { TRACEBACK_MIME: payload },
        });
        let ue = UserErrorOutput::from_nbformat(&v).unwrap();
        assert!(matches!(ue, UserErrorOutput::Rich(_)));
    }

    #[test]
    fn pep657_caret_rails_dropped_from_structured_lines_kept_in_text() {
        // Python 3.11+ adds fine-grained carets under the failing source
        // line. IPython's traceback includes line numbers on source
        // lines; the caret rail is an unnumbered sibling. Our parser:
        //   - captures the numbered source line (with highlight),
        //   - `is_caret_rail` drops the `~^^^` rail from structured
        //     `frame.lines`,
        //   - but the caret rail IS in `rt.text` since that comes from
        //     the ANSI-stripped join of the original traceback.
        let tb = vec![
            "  File \"/tmp/x.py\", line 1, in <module>".to_string(),
            "----> 1 a[b] + c".to_string(),
            "        ~^^^".to_string(),
            "TypeError: unsupported operand type(s)".to_string(),
        ];
        let rt = parse_ansi_traceback("TypeError", "unsupported operand type(s)", &tb).unwrap();
        let lines = rt.frames[0]
            .lines
            .as_ref()
            .expect("numbered source line should be captured");
        // The source line is present and flagged as highlighted.
        let src = lines
            .iter()
            .find(|l| l.lineno == 1)
            .expect("line 1 source captured");
        assert!(src.highlight);
        assert!(src.source.contains("a[b]"));
        // No structured-lines entry is a pure caret rail.
        assert!(!lines
            .iter()
            .any(|l| l.source.trim().chars().all(|c| c == '^' || c == '~')));
        // The caret rail is visible in the copy-ready text, though.
        assert!(rt.text.contains("~^^^"));
    }
}
