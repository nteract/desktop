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
//! Parsing is line-based on purpose. Adding `toml` / `serde_yaml` /
//! `pathdiff` to `runtimed` just for the commit-2 extraction round
//! costs more than it buys: the values we surface are simple (dep
//! lists, channels, pip sublist, requires-python), and matching the
//! existing repo style (`extract_pyproject_deps`, `pixi_toml_has_ipykernel`)
//! keeps the dep graph flat.

use std::path::Path;
use std::sync::Arc;

use tracing::{debug, warn};

use runtime_doc::{
    ProjectContext, ProjectFile, ProjectFileExtras, ProjectFileKind, ProjectFileParsed,
};

use crate::project_file::{self as daemon_project_file, DetectedProjectFile};

use super::room::NotebookRoom;

/// Cross-module entrypoint for the save-as handler. Save-as hands us a
/// concrete path (no `Option`), and lives in `crate::requests`, outside
/// `notebook_sync_server`'s `pub(super)` visibility.
pub(crate) fn refresh_project_context_on_save_as(room: &Arc<NotebookRoom>, canonical: &Path) {
    refresh_project_context(room, Some(canonical));
}

/// Walk up from the notebook path, parse what the daemon can, and write
/// the result into the room's `RuntimeStateDoc.project_context`.
///
/// Untitled notebooks (no path) leave the field at `Pending`; a sentinel
/// write would be misleading because there's nothing to refresh against.
///
/// Re-runnable: the caller may invoke this whenever the room's on-disk
/// path changes (untitled promotion, save-as rename). The setter clears
/// variant-specific fields before writing, so a `Detected` → `NotFound`
/// transition doesn't leave ghost data.
pub(super) fn refresh_project_context(room: &Arc<NotebookRoom>, path: Option<&Path>) {
    let Some(notebook_path) = path else {
        return;
    };

    let ctx = build_context(notebook_path);

    // RuntimeStateHandle.with_doc uses a std::sync::Mutex, no .await inside.
    if let Err(e) = room.state.with_doc(|sd| sd.set_project_context(&ctx)) {
        warn!(
            "[notebook-sync] Failed to write project_context for {:?}: {}",
            notebook_path, e
        );
        return;
    }

    debug!(
        "[notebook-sync] Wrote project_context for {:?}: {}",
        notebook_path,
        ctx.variant_str()
    );
}

/// Detect + parse, translating the daemon's internal `DetectedProjectFile`
/// into the `ProjectContext` shape consumers read.
fn build_context(notebook_path: &Path) -> ProjectContext {
    let observed_at = current_iso_timestamp();

    let Some(detected) = daemon_project_file::detect_project_file(notebook_path) else {
        return ProjectContext::NotFound { observed_at };
    };

    let kind = translate_kind(&detected.kind);
    let relative_to_notebook = relative_to_notebook(notebook_path, &detected.path);

    let content = match std::fs::read_to_string(&detected.path) {
        Ok(s) => s,
        Err(e) => {
            return ProjectContext::Unreadable {
                path: detected.path.to_string_lossy().into_owned(),
                reason: format!("read failed: {e}"),
                observed_at,
            };
        }
    };

    match parse_detected(&detected, &content) {
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
    }
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
        daemon_project_file::ProjectFileKind::PyprojectToml => Ok(parse_pyproject_toml(content)),
        daemon_project_file::ProjectFileKind::PixiToml => Ok(parse_pixi_toml(content)),
        daemon_project_file::ProjectFileKind::EnvironmentYml => {
            parse_environment_yml(&detected.path, content)
        }
    }
}

/// pyproject.toml: extract `[project].dependencies` + `[project].requires-python`.
/// Matches the style of `metadata::extract_pyproject_deps`.
fn parse_pyproject_toml(content: &str) -> ProjectFileParsed {
    let mut in_project = false;
    let mut in_deps = false;
    let mut deps: Vec<String> = Vec::new();
    let mut requires_python: Option<String> = None;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_deps = false;
            in_project = trimmed == "[project]";
            continue;
        }
        if !in_project {
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("requires-python") {
            if let Some(v) = extract_toml_string_value(value) {
                requires_python = Some(v);
            }
            continue;
        }
        if trimmed == "dependencies = [" || trimmed.starts_with("dependencies = [") {
            in_deps = true;
            if let Some(rest) = trimmed.strip_prefix("dependencies = [") {
                if let Some(inner) = rest.strip_suffix(']') {
                    push_string_entries(inner, &mut deps);
                    in_deps = false;
                }
            }
            continue;
        }
        if in_deps {
            if trimmed == "]" || trimmed.starts_with(']') {
                in_deps = false;
                continue;
            }
            if let Some(dep) = extract_quoted_entry(trimmed) {
                deps.push(dep);
            }
        }
    }

    ProjectFileParsed {
        dependencies: deps,
        requires_python,
        prerelease: None,
        extras: ProjectFileExtras::None,
    }
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
        requires_python: config.python,
        prerelease: None,
        extras: ProjectFileExtras::EnvironmentYml { pip },
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

fn extract_toml_string_value(assignment_tail: &str) -> Option<String> {
    let eq_tail = assignment_tail.trim_start();
    let eq_tail = eq_tail.strip_prefix('=')?.trim_start();
    let (quote, rest) = if let Some(rest) = eq_tail.strip_prefix('"') {
        ('"', rest)
    } else if let Some(rest) = eq_tail.strip_prefix('\'') {
        ('\'', rest)
    } else {
        return None;
    };
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
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
        let ctx = build_context(&notebook);
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

        let ctx = build_context(&notebook);
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
    fn build_context_pyproject_multiline_deps() {
        let temp = TempDir::new().unwrap();
        write(
            temp.path(),
            "pyproject.toml",
            "[project]\nname = \"demo\"\ndependencies = [\n    \"pandas>=2.0\",\n    \"numpy\",\n]\n",
        );
        let notebook = write(temp.path(), "demo.ipynb", "{}");

        let ctx = build_context(&notebook);
        let ProjectContext::Detected { parsed, .. } = ctx else {
            panic!("expected Detected");
        };
        assert_eq!(parsed.dependencies, vec!["pandas>=2.0", "numpy"]);
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

        let ctx = build_context(&notebook);
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

        let ctx = build_context(&notebook);
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
        assert_eq!(parsed.requires_python.as_deref(), Some("3.11"));
        let ProjectFileExtras::EnvironmentYml { pip } = parsed.extras else {
            panic!("expected EnvironmentYml extras");
        };
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

        let ctx = build_context(&notebook);
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

        let ctx = build_context(&notebook);
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
        let first = build_context(&nb_in_project);
        assert!(
            matches!(first, ProjectContext::Detected { .. }),
            "expected Detected for notebook under project dir, got {first:?}"
        );

        // Save-as to a bare directory: should now be NotFound, with no
        // leaked fields from the earlier Detected. `set_project_context`
        // does the field-clearing in the CRDT setter; here we just
        // confirm `build_context` returns the unambiguous answer for
        // the new path.
        let second = build_context(&nb_no_project);
        assert!(
            matches!(second, ProjectContext::NotFound { .. }),
            "expected NotFound for notebook in bare dir, got {second:?}"
        );
    }
}
