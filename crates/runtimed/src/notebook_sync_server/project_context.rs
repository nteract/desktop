//! Populate `RuntimeStateDoc.project_context` on notebook open.
//!
//! The daemon is the sole writer of this field (see high-risk
//! architecture invariants: single-writer per shared CRDT key). This
//! module is the only place in the daemon that calls
//! `set_project_context`; clients read via the normal RuntimeState sync
//! path.
//!
//! Write triggers:
//!
//! - Room creation (`get_or_create_room` in `catalog.rs`).
//! - Untitled → file-backed promotion, after `finalize_untitled_promotion`
//!   stamps the new path.
//! - Save-as rename, after `save_notebook_to_disk` moves the file and
//!   `set_path` updates the room.
//!
//! Filesystem watches for external project-file edits or for the
//! notebook being moved out from under us are still follow-up work.
//!
//! pyproject.toml goes through the `toml` crate; pixi.toml and
//! environment.yml still use line-scan for the handful of fields we
//! surface. We reach for real parsing when the format's grammar bites
//! back (PEP 508 extras like `requests[security,socks]>=2` contain
//! commas inside brackets, which a line scanner can't unambiguously
//! split).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use notify_debouncer_mini::DebounceEventResult;
use tokio::sync::oneshot;
use tracing::{debug, error, info, warn};

use runtime_doc::{
    ProjectContext, ProjectFile, ProjectFileExtras, ProjectFileKind, ProjectFileParsed,
};

use crate::project_file::{self as daemon_project_file, DetectedProjectFile};

use super::room::NotebookRoom;
use crate::task_supervisor::spawn_best_effort;

/// Walk up from the notebook path, parse what the daemon can, write the
/// result into `RuntimeStateDoc.project_context`, and arm (or rearm) a
/// filesystem watcher on the detected project file. External edits to
/// that file will re-enter this routine via the watcher.
///
/// Untitled notebooks (no path) leave the field at `Pending`; a sentinel
/// write would be misleading because there's nothing to refresh against.
///
/// Re-runnable: the caller may invoke this whenever the room's on-disk
/// path changes (untitled promotion, save-as rename). The setter clears
/// variant-specific fields before writing, so a `Detected` → `NotFound`
/// transition doesn't leave ghost data. Any previously-armed watcher is
/// shut down before a new one is spawned.
pub(super) async fn refresh_project_context_async(room: &Arc<NotebookRoom>, path: Option<&Path>) {
    let (ctx, detected_path) = match path {
        Some(p) => build_context(p),
        None => return,
    };

    if let Err(e) = room.state.with_doc(|sd| sd.set_project_context(&ctx)) {
        warn!(
            "[notebook-sync] Failed to write project_context for {:?}: {}",
            path, e
        );
        return;
    }

    debug!(
        "[notebook-sync] Wrote project_context for {:?}: {}",
        path,
        ctx.variant_str()
    );

    rearm_project_file_watcher(room, detected_path).await;
}

/// Shut down any existing project-file watcher on the room and arm a new
/// one pointed at the just-detected path (if any). A `None` detection
/// result (NotFound / Unreadable with missing file) leaves the slot
/// empty; the next refresh trigger will re-evaluate.
async fn rearm_project_file_watcher(room: &Arc<NotebookRoom>, detected_path: Option<PathBuf>) {
    room.file_binding.shutdown_project_file_watcher().await;

    let Some(watch_path) = detected_path else {
        return;
    };

    let (shutdown_tx, ready_rx) = spawn_project_file_watcher(watch_path, room.clone());
    room.file_binding
        .install_project_file_watcher_shutdown_tx(shutdown_tx)
        .await;
    // Block until the watcher task has actually installed its
    // subscription (or failed trying). Without this, events that land
    // between "we returned from this function" and "notify attached
    // itself" go unreported. On the failure path the sender is dropped
    // and the receiver returns `Err(RecvError)` — we just continue,
    // because the task has already logged the error and there's no
    // watcher to wait on anyway.
    let _ = ready_rx.await;
}

/// Watch a single project file and call `refresh_project_context_async`
/// whenever the debouncer fires a change. The daemon is not a writer
/// of project files today, so there's no self-write feedback loop to
/// suppress — any event from `notify` is an external change worth
/// re-parsing.
///
/// Returns two channels:
///
/// - `shutdown_tx`: the caller stores it; sending (or dropping) stops
///   the watcher.
/// - `ready_rx`: resolves when the task has actually installed the
///   FSEvents subscription (or hit an error and given up). Callers
///   await this before returning to guarantee that events after the
///   return point actually reach the watcher.
fn spawn_project_file_watcher(
    project_file_path: PathBuf,
    room: Arc<NotebookRoom>,
) -> (oneshot::Sender<()>, oneshot::Receiver<()>) {
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let (ready_tx, ready_rx) = oneshot::channel::<()>();

    spawn_best_effort("project-file-watcher", async move {
        // Signals readiness or gives up, running `ready_tx.send(())`
        // exactly once on any exit path. Dropping without sending (the
        // error arms below) still unblocks the waiter via `RecvError`.
        let mut ready_tx = Some(ready_tx);
        let signal_ready = |slot: &mut Option<oneshot::Sender<()>>| {
            if let Some(tx) = slot.take() {
                let _ = tx.send(());
            }
        };

        let (tx, mut rx) = tokio::sync::mpsc::channel::<DebounceEventResult>(16);
        let debouncer_result = notify_debouncer_mini::new_debouncer(
            std::time::Duration::from_millis(500),
            move |res: DebounceEventResult| {
                let _ = tx.blocking_send(res);
            },
        );

        let mut debouncer = match debouncer_result {
            Ok(d) => d,
            Err(e) => {
                error!(
                    "[project-watch] Failed to create watcher for {:?}: {}",
                    project_file_path, e
                );
                return;
            }
        };

        // Watch the parent directory. Watching the file directly doesn't
        // work on macOS FSEvents for atomic "write to temp + rename"
        // sequences — the inode the watch is pinned to disappears. The
        // parent-dir watch catches all modify / create / remove events
        // for our file and we filter on path below.
        let Some(parent_dir) = project_file_path.parent() else {
            error!(
                "[project-watch] Project file {:?} has no parent dir",
                project_file_path
            );
            return;
        };

        if let Err(e) = debouncer
            .watcher()
            .watch(parent_dir, notify::RecursiveMode::NonRecursive)
        {
            error!("[project-watch] Failed to watch {:?}: {}", parent_dir, e);
            return;
        }

        // Canonicalize the target path once so we can compare against
        // the canonical paths `notify` emits on platforms that resolve
        // symlinks (macOS's /var → /private/var). Falls back to the
        // original path when canonicalize fails (file may not exist).
        let canonical_target =
            std::fs::canonicalize(&project_file_path).unwrap_or_else(|_| project_file_path.clone());

        info!(
            "[project-watch] Watching project file {:?} (parent {:?})",
            project_file_path, parent_dir
        );

        // Subscription is live; unblock the waiter. From this point on,
        // events that land against `project_file_path` reach `rx`.
        signal_ready(&mut ready_tx);

        loop {
            tokio::select! {
                Some(result) = rx.recv() => {
                    match result {
                        Ok(events) => {
                            let relevant = events.iter().any(|e| {
                                e.path == project_file_path || e.path == canonical_target
                            });
                            if !relevant {
                                continue;
                            }
                            // Re-run detection against the room's current
                            // notebook path, not the cached project-file
                            // path: the notebook may have been moved such
                            // that a closer project file now wins, or the
                            // detected file may have been deleted.
                            let notebook_path = room.file_binding.path().await;
                            refresh_project_context_async(&room, notebook_path.as_deref()).await;
                        }
                        Err(e) => {
                            warn!(
                                "[project-watch] Debouncer error for {:?}: {:?}",
                                project_file_path, e
                            );
                        }
                    }
                }
                _ = &mut shutdown_rx => {
                    debug!(
                        "[project-watch] Shutting down watcher for {:?}",
                        project_file_path
                    );
                    return;
                }
            }
        }
    });

    (shutdown_tx, ready_rx)
}

/// Detect + parse, translating the daemon's internal `DetectedProjectFile`
/// into the `ProjectContext` shape consumers read. Also returns the
/// detected file's absolute path when there is one, so the caller can
/// (re)arm a filesystem watcher on it.
fn build_context(notebook_path: &Path) -> (ProjectContext, Option<PathBuf>) {
    let observed_at = current_iso_timestamp();

    let Some(detected) = daemon_project_file::detect_project_file(notebook_path) else {
        return (ProjectContext::NotFound { observed_at }, None);
    };

    let detected_path = detected.path.clone();
    let kind = translate_kind(&detected.kind);
    let relative_to_notebook = relative_to_notebook(notebook_path, &detected.path);

    let content = match std::fs::read_to_string(&detected.path) {
        Ok(s) => s,
        Err(e) => {
            return (
                ProjectContext::Unreadable {
                    path: detected.path.to_string_lossy().into_owned(),
                    reason: format!("read failed: {e}"),
                    observed_at,
                },
                Some(detected_path),
            );
        }
    };

    let ctx = match parse_detected(&detected, &content) {
        Ok(parsed) => ProjectContext::Detected {
            project_file: ProjectFile {
                kind,
                absolute_path: detected.path.to_string_lossy().into_owned(),
                relative_to_notebook,
            },
            parsed,
            observed_at,
        },
        Err(reason) => ProjectContext::Unreadable {
            path: detected.path.to_string_lossy().into_owned(),
            reason,
            observed_at,
        },
    };
    (ctx, Some(detected_path))
}

fn translate_kind(kind: &daemon_project_file::ProjectFileKind) -> ProjectFileKind {
    match kind {
        daemon_project_file::ProjectFileKind::PyprojectToml => ProjectFileKind::PyprojectToml,
        daemon_project_file::ProjectFileKind::PixiToml => ProjectFileKind::PixiToml,
        daemon_project_file::ProjectFileKind::EnvironmentYml => ProjectFileKind::EnvironmentYml,
    }
}

/// Best-effort relative path from the notebook's parent to the project
/// file. Falls back to the absolute path when a common ancestor can't
/// be cheaply derived. Purely display-oriented; consumers compare by
/// kind and absolute_path, not by this.
fn relative_to_notebook(notebook_path: &Path, project_file: &Path) -> String {
    let notebook_dir = notebook_path.parent().unwrap_or(notebook_path);
    // For the common case (notebook's directory contains or is the
    // ancestor of the project file), strip_prefix is all we need.
    if let Ok(rel) = project_file.strip_prefix(notebook_dir) {
        return rel.to_string_lossy().into_owned();
    }
    // Walk up from notebook_dir counting ".." hops until we hit an
    // ancestor that's also an ancestor of the project file.
    let mut hops = 0usize;
    let mut current = notebook_dir;
    loop {
        if let Ok(rel) = project_file.strip_prefix(current) {
            let mut out = String::new();
            for _ in 0..hops {
                out.push_str("../");
            }
            out.push_str(&rel.to_string_lossy());
            return out;
        }
        match current.parent() {
            Some(parent) if parent != current => {
                current = parent;
                hops += 1;
            }
            _ => break,
        }
    }
    project_file.to_string_lossy().into_owned()
}

fn parse_detected(
    detected: &DetectedProjectFile,
    content: &str,
) -> Result<ProjectFileParsed, String> {
    match detected.kind {
        daemon_project_file::ProjectFileKind::PyprojectToml => parse_pyproject_toml(content),
        daemon_project_file::ProjectFileKind::PixiToml => Ok(parse_pixi_toml(content)),
        daemon_project_file::ProjectFileKind::EnvironmentYml => {
            parse_environment_yml(&detected.path, content)
        }
    }
}

/// pyproject.toml: extract `[project].dependencies`,
/// `[project].requires-python`, and `[tool.uv].dev-dependencies`.
///
/// Uses real TOML parsing via `serde`. PEP 508 specs such as
/// `requests[security,socks]>=2` include commas inside bracket groups,
/// which a line scanner couldn't distinguish from list separators.
///
/// Parse errors — both raw-TOML syntax failures and schema-valid but
/// shape-invalid inputs (`dependencies = "pandas"` where a list is
/// expected) — route to `Err(reason)` so `build_context` emits
/// `ProjectContext::Unreadable` and the UI surfaces the problem instead
/// of silently reporting zero deps.
fn parse_pyproject_toml(content: &str) -> Result<ProjectFileParsed, String> {
    #[derive(serde::Deserialize, Default)]
    struct Root {
        #[serde(default)]
        project: ProjectTable,
        #[serde(default)]
        tool: ToolTable,
    }

    #[derive(serde::Deserialize, Default)]
    struct ProjectTable {
        #[serde(default)]
        dependencies: Vec<String>,
        #[serde(rename = "requires-python", default)]
        requires_python: Option<String>,
    }

    #[derive(serde::Deserialize, Default)]
    struct ToolTable {
        #[serde(default)]
        uv: ToolUv,
    }

    #[derive(serde::Deserialize, Default)]
    struct ToolUv {
        #[serde(rename = "dev-dependencies", default)]
        dev_dependencies: Vec<String>,
    }

    let root: Root =
        toml::from_str(content).map_err(|e| format!("pyproject.toml parse failed: {e}"))?;
    Ok(ProjectFileParsed {
        dependencies: root.project.dependencies,
        dev_dependencies: root.tool.uv.dev_dependencies,
        requires_python: root.project.requires_python,
        prerelease: None,
        extras: ProjectFileExtras::None,
    })
}

/// Commit-2 pixi parsing keeps the easy signals: channels (top-level
/// array) and the list of dep keys under `[dependencies]`/
/// `[pypi-dependencies]`. Structured values like `{ version = "^13" }`
/// are represented by the bare package name for now; commit 3 can
/// enrich with proper TOML parsing if we find consumers that need the
/// version specifiers.
fn parse_pixi_toml(content: &str) -> ProjectFileParsed {
    let mut current_table = String::new();
    let mut channels: Vec<String> = Vec::new();
    let mut deps: Vec<String> = Vec::new();
    let mut pypi: Vec<String> = Vec::new();
    let mut in_channels_array = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if in_channels_array {
            // collect entries until closing `]`
            if trimmed.starts_with(']') {
                in_channels_array = false;
                continue;
            }
            if let Some(entry) = extract_quoted_entry(trimmed) {
                channels.push(entry);
            }
            continue;
        }

        if let Some(header) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            current_table = header.to_string();
            continue;
        }

        // Top-level `channels = [...]` applies under pixi's `[project]`
        // or `[tool.pixi.project]` tables. Pick it up wherever it lands.
        if trimmed.starts_with("channels") && trimmed.contains('=') {
            if let Some((_, rest)) = trimmed.split_once('=') {
                let rest = rest.trim();
                if rest.starts_with('[') {
                    if let Some(inner) = rest.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                        push_string_entries(inner, &mut channels);
                    } else {
                        in_channels_array = true;
                    }
                }
            }
            continue;
        }

        // Dep-table entries: `name = "spec"` or `name = { ... }`. Commit
        // 2 surfaces the key only.
        let is_dep_table = current_table == "dependencies"
            || current_table == "tool.pixi.dependencies"
            || current_table == "pypi-dependencies"
            || current_table == "tool.pixi.pypi-dependencies";
        if is_dep_table {
            if let Some(key) = trimmed.split('=').next() {
                let key = key.trim();
                if !key.is_empty() && !key.starts_with('#') {
                    if current_table.ends_with("pypi-dependencies") {
                        pypi.push(key.to_string());
                    } else {
                        deps.push(key.to_string());
                    }
                }
            }
        }
    }

    deps.sort();
    pypi.sort();

    ProjectFileParsed {
        dependencies: deps,
        dev_dependencies: Vec::new(),
        requires_python: None,
        prerelease: None,
        extras: ProjectFileExtras::Pixi {
            channels,
            pypi_dependencies: pypi,
        },
    }
}

/// environment.yml parses via the daemon's existing rattler-backed
/// parser for deps/python. When that parse fails the file is genuinely
/// unreadable (bad YAML, bad conda spec); route the caller to
/// `ProjectContext::Unreadable` instead of emitting an empty `Detected`
/// that hides the problem.
fn parse_environment_yml(path: &Path, content: &str) -> Result<ProjectFileParsed, String> {
    let config = daemon_project_file::parse_environment_yml(path)?;
    let pip = extract_environment_yml_pip(content);

    Ok(ProjectFileParsed {
        dependencies: config.dependencies,
        dev_dependencies: Vec::new(),
        requires_python: config.python,
        prerelease: None,
        extras: ProjectFileExtras::EnvironmentYml {
            channels: config.channels,
            pip,
        },
    })
}

/// Line-scan for `  - pip:` followed by its nested `    - foo` entries.
/// Tracks indentation of the `pip:` marker to know when the block ends.
fn extract_environment_yml_pip(content: &str) -> Vec<String> {
    let mut pip: Vec<String> = Vec::new();
    let mut pip_indent: Option<usize> = None;

    for line in content.lines() {
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim();

        if pip_indent.is_some() {
            if trimmed.is_empty() {
                continue;
            }
            // A line whose indent is <= the `pip:` marker ends the block.
            if indent <= pip_indent.unwrap_or(usize::MAX) {
                pip_indent = None;
            } else if let Some(rest) = trimmed.strip_prefix("- ") {
                let entry = rest.trim().trim_matches('"').trim_matches('\'').trim();
                if !entry.is_empty() {
                    pip.push(entry.to_string());
                }
                continue;
            }
        }

        if trimmed == "- pip:" {
            pip_indent = Some(indent);
        }
    }

    pip
}

fn extract_quoted_entry(line: &str) -> Option<String> {
    let trimmed = line.trim().trim_end_matches(',').trim();
    let (quote, rest) = if let Some(rest) = trimmed.strip_prefix('"') {
        ('"', rest)
    } else if let Some(rest) = trimmed.strip_prefix('\'') {
        ('\'', rest)
    } else {
        return None;
    };
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}

fn push_string_entries(inner: &str, dest: &mut Vec<String>) {
    for raw in inner.split(',') {
        if let Some(entry) = extract_quoted_entry(raw) {
            if !entry.is_empty() {
                dest.push(entry);
            }
        }
    }
}

fn current_iso_timestamp() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn build_context_on_empty_tempdir_returns_not_found() {
        let temp = TempDir::new().unwrap();
        let notebook = write(temp.path(), "untitled.ipynb", "{}");
        let (ctx, _) = build_context(&notebook);
        assert!(matches!(ctx, ProjectContext::NotFound { .. }));
    }

    #[test]
    fn build_context_pyproject_extracts_deps_and_python() {
        let temp = TempDir::new().unwrap();
        write(
            temp.path(),
            "pyproject.toml",
            "[project]\nname = \"demo\"\ndependencies = [\"pandas>=2.0\", \"numpy\"]\nrequires-python = \">=3.11\"\n",
        );
        let notebook = write(temp.path(), "demo.ipynb", "{}");

        let (ctx, _) = build_context(&notebook);
        let ProjectContext::Detected {
            project_file,
            parsed,
            ..
        } = ctx
        else {
            panic!("expected Detected");
        };
        assert_eq!(project_file.kind, ProjectFileKind::PyprojectToml);
        assert_eq!(parsed.dependencies, vec!["pandas>=2.0", "numpy"]);
        assert_eq!(parsed.requires_python.as_deref(), Some(">=3.11"));
        assert_eq!(parsed.extras, ProjectFileExtras::None);
    }

    #[test]
    fn build_context_pyproject_preserves_pep508_extras() {
        // PEP 508 specs can nest brackets with commas. A line-scan
        // parser used to split "requests[security,socks]>=2" into two
        // pieces on the inner comma. The toml-backed parser keeps the
        // spec verbatim.
        let temp = TempDir::new().unwrap();
        write(
            temp.path(),
            "pyproject.toml",
            "[project]\nname = \"demo\"\ndependencies = [\n    \"requests[security,socks]>=2\",\n    \"httpx[http2]>=0.27\",\n    \"numpy\",\n]\n",
        );
        let notebook = write(temp.path(), "demo.ipynb", "{}");

        let (ctx, _) = build_context(&notebook);
        let ProjectContext::Detected { parsed, .. } = ctx else {
            panic!("expected Detected");
        };
        assert_eq!(
            parsed.dependencies,
            vec![
                "requests[security,socks]>=2".to_string(),
                "httpx[http2]>=0.27".to_string(),
                "numpy".to_string(),
            ]
        );
    }

    #[test]
    fn build_context_pyproject_preserves_env_marker_specs() {
        // Environment markers — "; python_version < '3.11'" — must
        // survive intact. Line-scanning used to truncate at the
        // semicolon under some multi-line formatters.
        let temp = TempDir::new().unwrap();
        write(
            temp.path(),
            "pyproject.toml",
            "[project]\nname = \"demo\"\ndependencies = [\n    \"tomli ; python_version < '3.11'\",\n]\n",
        );
        let notebook = write(temp.path(), "demo.ipynb", "{}");

        let (ctx, _) = build_context(&notebook);
        let ProjectContext::Detected { parsed, .. } = ctx else {
            panic!("expected Detected");
        };
        assert_eq!(
            parsed.dependencies,
            vec!["tomli ; python_version < '3.11'".to_string()]
        );
    }

    #[test]
    fn build_context_pyproject_multiline_deps() {
        let temp = TempDir::new().unwrap();
        write(
            temp.path(),
            "pyproject.toml",
            "[project]\nname = \"demo\"\ndependencies = [\n    \"pandas>=2.0\",\n    \"numpy\",\n]\n",
        );
        let notebook = write(temp.path(), "demo.ipynb", "{}");

        let (ctx, _) = build_context(&notebook);
        let ProjectContext::Detected { parsed, .. } = ctx else {
            panic!("expected Detected");
        };
        assert_eq!(parsed.dependencies, vec!["pandas>=2.0", "numpy"]);
    }

    #[test]
    fn build_context_pyproject_captures_tool_uv_dev_dependencies() {
        let temp = TempDir::new().unwrap();
        write(
            temp.path(),
            "pyproject.toml",
            "[project]\nname = \"demo\"\ndependencies = [\"pandas\"]\n\n[tool.uv]\ndev-dependencies = [\"pytest\", \"ruff>=0.6\"]\n",
        );
        let notebook = write(temp.path(), "demo.ipynb", "{}");

        let (ctx, _) = build_context(&notebook);
        let ProjectContext::Detected { parsed, .. } = ctx else {
            panic!("expected Detected");
        };
        assert_eq!(parsed.dependencies, vec!["pandas"]);
        assert_eq!(parsed.dev_dependencies, vec!["pytest", "ruff>=0.6"]);
    }

    #[test]
    fn build_context_pyproject_schema_mismatch_routes_to_unreadable() {
        // Well-formed TOML, wrong shape: `dependencies` should be a
        // list of strings, not a bare string. The prior fallback
        // silently reported zero deps — now it surfaces as Unreadable
        // so the UI can explain what's wrong.
        let temp = TempDir::new().unwrap();
        write(
            temp.path(),
            "pyproject.toml",
            "[project]\nname = \"demo\"\ndependencies = \"pandas\"\n",
        );
        let notebook = write(temp.path(), "demo.ipynb", "{}");

        let (ctx, _) = build_context(&notebook);
        let ProjectContext::Unreadable { path, reason, .. } = ctx else {
            panic!("expected Unreadable, got {ctx:?}");
        };
        assert!(path.ends_with("pyproject.toml"));
        assert!(reason.contains("parse failed"));
    }

    #[test]
    fn build_context_pyproject_empty_dev_deps_when_tool_uv_absent() {
        // Plain [project] pyproject with no [tool.uv] block. Not having
        // dev-dependencies shouldn't leak values from a prior state; it
        // should be an empty vec.
        let temp = TempDir::new().unwrap();
        write(
            temp.path(),
            "pyproject.toml",
            "[project]\nname = \"demo\"\ndependencies = [\"pandas\"]\n",
        );
        let notebook = write(temp.path(), "demo.ipynb", "{}");

        let (ctx, _) = build_context(&notebook);
        let ProjectContext::Detected { parsed, .. } = ctx else {
            panic!("expected Detected");
        };
        assert!(parsed.dev_dependencies.is_empty());
    }

    #[test]
    fn build_context_pixi_collects_channels_and_pypi_keys() {
        let temp = TempDir::new().unwrap();
        write(
            temp.path(),
            "pixi.toml",
            "[project]\nname = \"demo\"\nchannels = [\"conda-forge\", \"bioconda\"]\n\n[dependencies]\npython = \"3.11.*\"\nnumpy = \"*\"\n\n[pypi-dependencies]\nrequests = \"*\"\nrich = { version = \"^13\" }\n",
        );
        let notebook = write(temp.path(), "demo.ipynb", "{}");

        let (ctx, _) = build_context(&notebook);
        let ProjectContext::Detected {
            project_file,
            parsed,
            ..
        } = ctx
        else {
            panic!("expected Detected");
        };
        assert_eq!(project_file.kind, ProjectFileKind::PixiToml);
        assert!(parsed.dependencies.iter().any(|d| d == "numpy"));
        assert!(parsed.dependencies.iter().any(|d| d == "python"));
        let ProjectFileExtras::Pixi {
            channels,
            pypi_dependencies,
        } = parsed.extras
        else {
            panic!("expected Pixi extras");
        };
        assert_eq!(channels, vec!["conda-forge", "bioconda"]);
        assert!(pypi_dependencies.iter().any(|d| d == "requests"));
        assert!(pypi_dependencies.iter().any(|d| d == "rich"));
    }

    #[test]
    fn build_context_environment_yml_pulls_pip_sublist() {
        let temp = TempDir::new().unwrap();
        write(
            temp.path(),
            "environment.yml",
            "name: demo\nchannels:\n  - conda-forge\ndependencies:\n  - python=3.11\n  - numpy\n  - pip:\n    - requests\n    - flask\n",
        );
        let notebook = write(temp.path(), "demo.ipynb", "{}");

        let (ctx, _) = build_context(&notebook);
        let ProjectContext::Detected {
            project_file,
            parsed,
            ..
        } = ctx
        else {
            panic!("expected Detected");
        };
        assert_eq!(project_file.kind, ProjectFileKind::EnvironmentYml);
        assert!(parsed.dependencies.iter().any(|d| d == "numpy"));
        // rattler normalizes `python=3.11` → `"3.11.*"`
        assert_eq!(parsed.requires_python.as_deref(), Some("3.11.*"));
        let ProjectFileExtras::EnvironmentYml { channels, pip } = parsed.extras else {
            panic!("expected EnvironmentYml extras");
        };
        assert_eq!(channels, vec!["conda-forge".to_string()]);
        assert_eq!(pip, vec!["requests".to_string(), "flask".to_string()]);
    }

    #[test]
    fn build_context_malformed_environment_yml_returns_unreadable() {
        let temp = TempDir::new().unwrap();
        // Invalid YAML — the rattler parser rejects it.
        write(
            temp.path(),
            "environment.yml",
            "name: demo\ndependencies: { this is not valid yaml\n",
        );
        let notebook = write(temp.path(), "demo.ipynb", "{}");

        let (ctx, _) = build_context(&notebook);
        let ProjectContext::Unreadable { path, reason, .. } = ctx else {
            panic!("expected Unreadable, got {ctx:?}");
        };
        assert!(path.ends_with("environment.yml"));
        assert!(!reason.is_empty());
    }

    #[test]
    fn build_context_unreadable_on_read_error() {
        // Create a pyproject.toml as a directory instead of a file to
        // force a read failure. Real cases (permissions, vanished file)
        // hit the same Err arm.
        let temp = TempDir::new().unwrap();
        std::fs::create_dir(temp.path().join("pyproject.toml")).unwrap();
        let notebook = write(temp.path(), "demo.ipynb", "{}");

        let (ctx, _) = build_context(&notebook);
        let ProjectContext::Unreadable { path, reason, .. } = ctx else {
            panic!("expected Unreadable, got {ctx:?}");
        };
        assert!(path.ends_with("pyproject.toml"));
        assert!(reason.contains("read failed"));
    }

    #[test]
    fn relative_to_notebook_handles_sibling_and_parent() {
        assert_eq!(
            relative_to_notebook(
                Path::new("/foo/bar/demo.ipynb"),
                Path::new("/foo/bar/pyproject.toml"),
            ),
            "pyproject.toml"
        );
        assert_eq!(
            relative_to_notebook(
                Path::new("/foo/bar/demo.ipynb"),
                Path::new("/foo/pyproject.toml"),
            ),
            "../pyproject.toml"
        );
    }

    #[test]
    fn build_context_reflects_new_location_on_rerun() {
        // Locks in the "re-runnable on path change" promise in
        // refresh_project_context's doc comment. Building against
        // location A then rebuilding against location B must produce
        // B's answer, not a merge of both.
        let temp = TempDir::new().unwrap();
        let with_pyproject = temp.path().join("with");
        let bare = temp.path().join("bare");
        std::fs::create_dir_all(&with_pyproject).unwrap();
        std::fs::create_dir_all(&bare).unwrap();
        write(
            &with_pyproject,
            "pyproject.toml",
            "[project]\nname = \"demo\"\ndependencies = [\"pandas\"]\n",
        );

        let nb_in_project = write(&with_pyproject, "demo.ipynb", "{}");
        let nb_no_project = write(&bare, "demo.ipynb", "{}");

        // First location: Detected against pyproject.toml.
        let (first, _) = build_context(&nb_in_project);
        assert!(
            matches!(first, ProjectContext::Detected { .. }),
            "expected Detected for notebook under project dir, got {first:?}"
        );

        // Save-as to a bare directory: should now be NotFound, with no
        // leaked fields from the earlier Detected. `set_project_context`
        // does the field-clearing in the CRDT setter; here we just
        // confirm `build_context` returns the unambiguous answer for
        // the new path.
        let (second, _) = build_context(&nb_no_project);
        assert!(
            matches!(second, ProjectContext::NotFound { .. }),
            "expected NotFound for notebook in bare dir, got {second:?}"
        );
    }
}
