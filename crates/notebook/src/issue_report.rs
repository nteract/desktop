use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

const LOG_TAIL_MAX_LINES: usize = 120;
const LOG_TAIL_MAX_CHARS: usize = 4000;
const TRUNCATED_MARKER: &str = "[truncated]\n";
const UNAVAILABLE: &str = "unavailable";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueReportPayload {
    pub app_version: String,
    pub app_commit: String,
    pub app_release_date: String,
    pub os: String,
    pub platform: String,
    pub arch: String,
    pub daemon_version: String,
    pub daemon_socket_mode: String,
    pub daemon_socket_path: String,
    pub daemon_log_tail: String,
    pub diagnostics_markdown: String,
}

pub fn prepare_issue_report_payload() -> IssueReportPayload {
    let home_dir = dirs::home_dir();
    let (daemon_version, daemon_socket_mode, daemon_socket_path) =
        collect_daemon_details(home_dir.as_deref());
    let daemon_log_tail = read_sanitized_log_tail_from_path(
        &runtimed::default_log_path(),
        home_dir.as_deref(),
        LOG_TAIL_MAX_LINES,
        LOG_TAIL_MAX_CHARS,
    );

    let mut payload = IssueReportPayload {
        app_version: crate::menu::APP_VERSION.to_string(),
        app_commit: crate::menu::APP_COMMIT_SHA.to_string(),
        app_release_date: crate::menu::APP_RELEASE_DATE.to_string(),
        os: std::env::consts::OS.to_string(),
        platform: std::env::consts::FAMILY.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        daemon_version,
        daemon_socket_mode,
        daemon_socket_path,
        daemon_log_tail,
        diagnostics_markdown: String::new(),
    };
    payload.diagnostics_markdown = render_diagnostics_markdown(&payload);
    payload
}

fn collect_daemon_details(home_dir: Option<&Path>) -> (String, String, String) {
    match runtimed::singleton::get_running_daemon_info() {
        Some(info) => (
            info.version,
            daemon_socket_mode(&info.endpoint).to_string(),
            sanitize_text_for_report(&info.endpoint, home_dir),
        ),
        None => (
            UNAVAILABLE.to_string(),
            UNAVAILABLE.to_string(),
            UNAVAILABLE.to_string(),
        ),
    }
}

fn daemon_socket_mode(endpoint: &str) -> &'static str {
    if endpoint.starts_with(r"\\.\pipe\") {
        "named_pipe"
    } else if endpoint.is_empty() {
        UNAVAILABLE
    } else {
        "unix_socket"
    }
}

pub(crate) fn read_sanitized_log_tail_from_path(
    log_path: &Path,
    home_dir: Option<&Path>,
    max_lines: usize,
    max_chars: usize,
) -> String {
    match read_log_tail(log_path, max_lines, max_chars) {
        Ok(log_tail) => {
            if log_tail.trim().is_empty() {
                "unavailable: daemon log is empty".to_string()
            } else {
                let sanitized = sanitize_text_for_report(&log_tail, home_dir);
                truncate_to_char_cap(&sanitized, max_chars)
            }
        }
        Err(error) => format!("unavailable: {error}"),
    }
}

fn read_log_tail(log_path: &Path, max_lines: usize, max_chars: usize) -> Result<String, String> {
    let file = File::open(log_path).map_err(|error| format!("daemon log unavailable ({error})"))?;
    let reader = BufReader::new(file);
    let mut tail = VecDeque::new();

    for line in reader.lines() {
        let line = line.map_err(|error| format!("daemon log unreadable ({error})"))?;
        if tail.len() == max_lines {
            tail.pop_front();
        }
        tail.push_back(line);
    }

    Ok(extract_bounded_tail_from_lines(tail, max_lines, max_chars))
}

pub(crate) fn extract_bounded_tail_from_lines<I, S>(
    lines: I,
    max_lines: usize,
    max_chars: usize,
) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    if max_lines == 0 || max_chars == 0 {
        return String::new();
    }

    let mut tail = VecDeque::new();
    for line in lines {
        if tail.len() == max_lines {
            tail.pop_front();
        }
        tail.push_back(line.as_ref().to_string());
    }
    let joined = tail.into_iter().collect::<Vec<_>>().join("\n");
    truncate_to_char_cap(&joined, max_chars)
}

fn truncate_to_char_cap(input: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let total_chars = input.chars().count();
    if total_chars <= max_chars {
        return input.to_string();
    }

    let marker_len = TRUNCATED_MARKER.chars().count();
    if max_chars <= marker_len {
        return TRUNCATED_MARKER.chars().take(max_chars).collect();
    }

    let suffix_len = max_chars - marker_len;
    let suffix: String = input.chars().skip(total_chars - suffix_len).collect();
    format!("{TRUNCATED_MARKER}{suffix}")
}

pub(crate) fn sanitize_text_for_report(input: &str, home_dir: Option<&Path>) -> String {
    let mut sanitized = input.to_string();

    if let Some(home) = home_dir {
        let home_path = home.to_string_lossy();
        if !home_path.is_empty() {
            sanitized = sanitized.replace(home_path.as_ref(), "~");
            let slash_normalized = home_path.replace('\\', "/");
            if slash_normalized != home_path {
                sanitized = sanitized.replace(&slash_normalized, "~");
            }
        }
    }

    sanitized = redact_user_segment(&sanitized, "/home/");
    sanitized = redact_user_segment(&sanitized, "/Users/");
    redact_user_segment(&sanitized, r"\Users\")
}

fn redact_user_segment(input: &str, prefix: &str) -> String {
    let separator = if prefix.contains('\\') { '\\' } else { '/' };
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;

    while let Some(offset) = input[cursor..].find(prefix) {
        let prefix_start = cursor + offset;
        let user_start = prefix_start + prefix.len();
        output.push_str(&input[cursor..user_start]);

        let remainder = &input[user_start..];
        let user_end = remainder.find(separator).unwrap_or(remainder.len());
        if user_end == 0 {
            cursor = user_start;
            continue;
        }

        output.push_str("<user>");
        cursor = user_start + user_end;
    }

    output.push_str(&input[cursor..]);
    output
}

pub(crate) fn render_diagnostics_markdown(payload: &IssueReportPayload) -> String {
    let safe_log_tail = payload.daemon_log_tail.replace("```", "'''");
    format!(
        "## Diagnostics\n\n\
- app version: `{}`\n\
- app commit: `{}`\n\
- app release date: `{}`\n\
- os: `{}`\n\
- platform: `{}`\n\
- arch: `{}`\n\
- daemon version: `{}`\n\
- daemon socket mode: `{}`\n\
- daemon socket path: `{}`\n\n\
### Daemon log tail (sanitized)\n\n\
```text\n{}\n```",
        payload.app_version,
        payload.app_commit,
        payload.app_release_date,
        payload.os,
        payload.platform,
        payload.arch,
        payload.daemon_version,
        payload.daemon_socket_mode,
        payload.daemon_socket_path,
        safe_log_tail
    )
}

#[cfg(test)]
mod tests {
    use super::{
        extract_bounded_tail_from_lines, read_sanitized_log_tail_from_path,
        render_diagnostics_markdown, sanitize_text_for_report, IssueReportPayload,
    };
    use std::path::{Path, PathBuf};

    #[test]
    fn redacts_home_prefix_and_username_segments() {
        let home = PathBuf::from("/home/alice");
        let input = "cwd=/home/alice/projects/demo.ipynb tmp=/home/bob/private/file.log";

        let sanitized = sanitize_text_for_report(input, Some(home.as_path()));

        assert!(sanitized.contains("~/projects/demo.ipynb"));
        assert!(sanitized.contains("/home/<user>/private/file.log"));
        assert!(!sanitized.contains("/home/alice"));
        assert!(!sanitized.contains("/home/bob"));
    }

    #[test]
    fn extracts_tail_and_applies_truncation_cap() {
        let lines = (0..10)
            .map(|index| format!("line-{index}"))
            .collect::<Vec<_>>();
        let no_truncation = extract_bounded_tail_from_lines(lines.clone(), 3, 100);
        assert_eq!(no_truncation, "line-7\nline-8\nline-9");

        let truncated = extract_bounded_tail_from_lines(lines, 10, 20);
        assert!(truncated.starts_with("[truncated]\n"));
        assert!(truncated.chars().count() <= 20);
    }

    #[test]
    fn markdown_includes_unavailable_log_marker() {
        let missing_log_tail = read_sanitized_log_tail_from_path(
            Path::new("/tmp/definitely-missing-runtimed-log.log"),
            None,
            50,
            500,
        );
        assert!(missing_log_tail.starts_with("unavailable:"));

        let payload = IssueReportPayload {
            app_version: "1.0.0".to_string(),
            app_commit: "abc123".to_string(),
            app_release_date: "2026-01-01".to_string(),
            os: "linux".to_string(),
            platform: "unix".to_string(),
            arch: "x86_64".to_string(),
            daemon_version: "unavailable".to_string(),
            daemon_socket_mode: "unavailable".to_string(),
            daemon_socket_path: "unavailable".to_string(),
            daemon_log_tail: missing_log_tail,
            diagnostics_markdown: String::new(),
        };

        let markdown = render_diagnostics_markdown(&payload);
        assert!(markdown.contains("Daemon log tail (sanitized)"));
        assert!(markdown.contains("unavailable:"));
    }
}
