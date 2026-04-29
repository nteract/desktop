//! Cached environment creation for inline dependencies.
//!
//! Delegates to `kernel_env` for the actual environment creation while
//! providing a [`BroadcastProgressHandler`] that records progress in
//! RuntimeStateDoc and forwards compatibility broadcast events.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use kernel_env::progress::{EnvProgressPhase, ProgressHandler};
use tokio::sync::broadcast;

use crate::protocol::NotebookBroadcast;
use runtime_doc::RuntimeStateHandle;

const CRDT_PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Default)]
struct CrdtProgressWriteState {
    last_phase: Option<&'static str>,
    last_write: Option<Instant>,
}

// Re-export the PreparedEnv-equivalent types for callers that still
// use the old `inline_env::PreparedEnv` pattern.
pub use kernel_env::conda::CondaEnvironment;
pub use kernel_env::uv::UvEnvironment;

/// Result of preparing an environment with inline deps.
#[derive(Debug, Clone)]
pub struct PreparedEnv {
    pub env_path: std::path::PathBuf,
    pub python_path: std::path::PathBuf,
}

/// Progress handler that broadcasts [`EnvProgressPhase`] events to all
/// connected notebook clients via a [`broadcast::Sender`].
pub struct BroadcastProgressHandler {
    tx: broadcast::Sender<NotebookBroadcast>,
    state: Option<RuntimeStateHandle>,
    crdt_write_state: Mutex<CrdtProgressWriteState>,
}

impl BroadcastProgressHandler {
    pub fn new(tx: broadcast::Sender<NotebookBroadcast>) -> Self {
        Self {
            tx,
            state: None,
            crdt_write_state: Mutex::default(),
        }
    }

    pub fn with_state(tx: broadcast::Sender<NotebookBroadcast>, state: RuntimeStateHandle) -> Self {
        Self {
            tx,
            state: Some(state),
            crdt_write_state: Mutex::default(),
        }
    }

    fn should_write_crdt_progress(&self, phase: &EnvProgressPhase) -> bool {
        let phase_name = env_progress_phase_name(phase);
        let high_frequency = matches!(
            phase,
            EnvProgressPhase::DownloadProgress { .. } | EnvProgressPhase::LinkProgress { .. }
        );

        let Ok(mut state) = self.crdt_write_state.lock() else {
            tracing::warn!("[runtime-state] failed to lock env progress throttle state");
            return true;
        };

        let now = Instant::now();
        let phase_changed = state.last_phase != Some(phase_name);
        let interval_elapsed = state
            .last_write
            .map(|last| now.duration_since(last) >= CRDT_PROGRESS_MIN_INTERVAL)
            .unwrap_or(true);

        if !high_frequency || phase_changed || interval_elapsed {
            state.last_phase = Some(phase_name);
            state.last_write = Some(now);
            return true;
        }

        false
    }
}

impl ProgressHandler for BroadcastProgressHandler {
    fn on_progress(&self, env_type: &str, phase: EnvProgressPhase) {
        // Log all phases
        kernel_env::LogHandler.on_progress(env_type, phase.clone());

        if let Some(state) = &self.state {
            if self.should_write_crdt_progress(&phase) {
                match serde_json::to_value(&phase) {
                    Ok(value) => {
                        if let Err(e) = state.with_doc(|sd| sd.set_env_progress(env_type, &value)) {
                            tracing::warn!("[runtime-state] failed to write env progress: {}", e);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("[runtime-state] failed to serialize env progress: {}", e);
                    }
                }
            }
        }

        // Broadcast to connected clients
        let _ = self.tx.send(NotebookBroadcast::EnvProgress {
            env_type: env_type.to_string(),
            phase,
        });
    }
}

fn env_progress_phase_name(phase: &EnvProgressPhase) -> &'static str {
    match phase {
        EnvProgressPhase::Starting { .. } => "starting",
        EnvProgressPhase::CacheHit { .. } => "cache_hit",
        EnvProgressPhase::LockFileHit => "lock_file_hit",
        EnvProgressPhase::OfflineHit => "offline_hit",
        EnvProgressPhase::FetchingRepodata { .. } => "fetching_repodata",
        EnvProgressPhase::RepodataComplete { .. } => "repodata_complete",
        EnvProgressPhase::Solving { .. } => "solving",
        EnvProgressPhase::SolveComplete { .. } => "solve_complete",
        EnvProgressPhase::Installing { .. } => "installing",
        EnvProgressPhase::DownloadProgress { .. } => "download_progress",
        EnvProgressPhase::LinkProgress { .. } => "link_progress",
        EnvProgressPhase::InstallComplete { .. } => "install_complete",
        EnvProgressPhase::CreatingVenv => "creating_venv",
        EnvProgressPhase::InstallingPackages { .. } => "installing_packages",
        EnvProgressPhase::Ready { .. } => "ready",
        EnvProgressPhase::Error { .. } => "error",
    }
}

/// Get the cache directory for inline dependency environments.
///
/// Channel-aware: shares `runt_workspace::daemon_base_dir` with the other
/// kernel-env caches so nightly and stable stay on their own trees.
pub(crate) fn inline_cache_dir() -> std::path::PathBuf {
    runt_workspace::daemon_base_dir().join("inline-envs")
}

/// Return inline deps unchanged.
///
/// Historically this appended `dx` when `bootstrap_dx` was set, so the
/// PyPI package would land in the inline env and the dep-hash would
/// distinguish bootstrap and non-bootstrap caches. Since 0.2.0 the
/// launcher package (which carries everything dx used to provide)
/// ships inside the daemon binary and is vendored via PYTHONPATH —
/// the inline env contents are identical either way, and bootstrap
/// vs non-bootstrap envs can share the cache.
///
/// Kept as a shim so callers don't all have to change on the same PR.
/// `bootstrap_dx` is accepted and ignored — plan to drop the parameter
/// once the field ripens into a pure launcher-module selector.
pub(crate) fn inline_deps_with_bootstrap(deps: &[String], _bootstrap_dx: bool) -> Vec<String> {
    deps.to_vec()
}

/// Prepare a cached UV environment with the given inline dependencies.
///
/// If a cached environment with the same deps already exists, returns it
/// immediately. Otherwise creates a new environment with uv venv + uv pip install.
///
/// `bootstrap_dx` is accepted for call-site compatibility and currently
/// ignored — launcher vendoring replaced the per-env `dx` PyPI install,
/// so bootstrap and non-bootstrap envs are identical and share the cache.
pub async fn prepare_uv_inline_env(
    deps: &[String],
    prerelease: Option<&str>,
    handler: Arc<dyn ProgressHandler>,
    bootstrap_dx: bool,
) -> Result<PreparedEnv> {
    let uv_deps = kernel_env::UvDependencies {
        dependencies: inline_deps_with_bootstrap(deps, bootstrap_dx),
        requires_python: Some(">=3.13".to_string()),
        prerelease: prerelease.map(|s| s.to_string()),
    };

    let env = kernel_env::uv::prepare_environment_in(&uv_deps, None, &inline_cache_dir(), handler)
        .await?;

    Ok(PreparedEnv {
        env_path: env.venv_path,
        python_path: env.python_path,
    })
}

/// Prepare a cached Conda environment with the given inline dependencies.
///
/// If a cached environment with the same deps+channels already exists, returns
/// it immediately. Otherwise creates a new environment using rattler.
pub async fn prepare_conda_inline_env(
    deps: &[String],
    channels: &[String],
    handler: Arc<dyn ProgressHandler>,
) -> Result<PreparedEnv> {
    let conda_deps = kernel_env::CondaDependencies {
        dependencies: deps.to_vec(),
        channels: if channels.is_empty() {
            vec!["conda-forge".to_string()]
        } else {
            channels.to_vec()
        },
        python: None,
        env_id: None,
    };

    let env = kernel_env::conda::prepare_environment_in(&conda_deps, &inline_cache_dir(), handler)
        .await?;

    Ok(PreparedEnv {
        env_path: env.env_path,
        python_path: env.python_path,
    })
}

/// Rename a pool-derived UV env to the inline-cache hash location so the
/// next launch with the same inline deps cache-hits via
/// [`check_uv_inline_cache`] instead of taking another pool env.
///
/// Idempotent and best-effort: skips when the env is already at the target,
/// when another flow beat us to the target path, or when the rename fails.
/// Updates `venv_path` / `python_path` on success so callers can continue
/// using the `PooledEnv` without thinking about the rename.
///
/// See #2089 / #2083: without this, a pool-reuse inline launch leaves the
/// env at `runtimed-uv-XXXX` and the next restart misses the inline cache,
/// takes a fresh pool env, and re-solves from scratch.
pub async fn claim_pool_env_for_uv_inline_cache(
    env: &mut crate::PooledEnv,
    deps: &[String],
    prerelease: Option<&str>,
    bootstrap_dx: bool,
) {
    let uv_deps = kernel_env::UvDependencies {
        dependencies: inline_deps_with_bootstrap(deps, bootstrap_dx),
        requires_python: Some(">=3.13".to_string()),
        prerelease: prerelease.map(|s| s.to_string()),
    };
    let hash = kernel_env::uv::compute_env_hash(&uv_deps, None);
    let target = inline_cache_dir().join(&hash);
    rename_env_to_target(&mut env.venv_path, &mut env.python_path, target).await;
}

/// Rename a pool-derived Conda env to the inline-cache hash location. See
/// [`claim_pool_env_for_uv_inline_cache`] for the rationale; same mechanism,
/// conda hash function.
pub async fn claim_pool_env_for_conda_inline_cache(
    env: &mut crate::PooledEnv,
    deps: &[String],
    channels: &[String],
) {
    let conda_deps = kernel_env::CondaDependencies {
        dependencies: deps.to_vec(),
        channels: if channels.is_empty() {
            vec!["conda-forge".to_string()]
        } else {
            channels.to_vec()
        },
        python: None,
        env_id: None,
    };
    let hash = kernel_env::conda::compute_env_hash(&conda_deps);
    let target = inline_cache_dir().join(&hash);
    rename_env_to_target(&mut env.venv_path, &mut env.python_path, target).await;
}

/// Shared rename logic: move `venv_path` to `target` and rewrite the python
/// path relative to the new root. Preserves the original `python_path`
/// layout (e.g. `bin/python` vs `Scripts/python.exe`).
async fn rename_env_to_target(
    venv_path: &mut std::path::PathBuf,
    python_path: &mut std::path::PathBuf,
    target: std::path::PathBuf,
) {
    if *venv_path == target {
        return; // already at target (e.g. prior claim)
    }
    if !venv_path.exists() {
        tracing::warn!(
            "[inline-env] claim_pool_env: source {:?} no longer exists, skipping rename",
            venv_path
        );
        return;
    }
    if target.exists() {
        // Concurrent build produced the same cache entry first. Leave our
        // env at the pool path; the next launch will cache-hit on their
        // entry and our pool path becomes orphan for the normal cleanup
        // paths. No correctness issue.
        tracing::info!(
            "[inline-env] claim_pool_env: target {:?} already exists, leaving env at {:?}",
            target,
            venv_path
        );
        return;
    }
    if let Some(parent) = target.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            tracing::warn!(
                "[inline-env] claim_pool_env: failed to create cache parent {:?}: {}",
                parent,
                e
            );
            return;
        }
    }
    // Preserve the python_path's layout relative to the old venv root.
    let rel_python = python_path
        .strip_prefix(&*venv_path)
        .ok()
        .map(|p| p.to_path_buf());
    match tokio::fs::rename(&*venv_path, &target).await {
        Ok(()) => {
            tracing::info!(
                "[inline-env] claim_pool_env: renamed {:?} -> {:?} for inline-cache reuse",
                venv_path,
                target
            );
            *venv_path = target.clone();
            if let Some(rel) = rel_python {
                *python_path = target.join(rel);
            }
        }
        Err(e) => {
            tracing::warn!(
                "[inline-env] claim_pool_env: rename {:?} -> {:?} failed: {}",
                venv_path,
                target,
                e
            );
        }
    }
}

/// Result of comparing inline deps against pool packages.
#[derive(Debug)]
pub enum PoolDepRelation {
    /// All inline deps are already installed in the pool env.
    Subset,
    /// Pool covers some deps; these extras need installing.
    Additive { delta: Vec<String> },
    /// Cannot determine compatibility (version pins, etc.) — build from scratch.
    Independent,
}

/// Split a dependency string into `(bare_name, constraint_tail)`.
///
/// The constraint tail preserves everything after the bare name —
/// version specifier, extras, AND any environment marker — with
/// whitespace squeezed out so variants like `pkg>=1.0` and
/// `pkg >= 1.0` normalize to the same tail. Empty tail means the
/// dep is bare and unconditional (accepts any version, always
/// installed). Returns `None` when the input is empty or produces
/// an empty bare name.
///
/// Why the marker stays in the tail: pool `prewarmed_packages` is
/// the install-spec list, not the actually-installed list. A spec
/// like `gremlin ; sys_platform == 'darwin'` means gremlin is only
/// present on Darwin pool envs. On other platforms the pool entry
/// exists but gremlin isn't installed. Matching against the
/// unconditional bare name would make a bare-`gremlin` inline dep
/// hit a pool env that lacks the package. Keeping the marker in the
/// tail forces a byte-equal match (or fall through to Independent).
fn split_bare_and_constraint(dep: &str) -> Option<(&str, String)> {
    let trimmed = dep.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Cut at the first specifier, marker, or whitespace — whichever
    // ends the bare name. The tail then carries everything from the
    // first specifier through the end of the original string (marker
    // included).
    let cut_chars = ['>', '<', '=', '!', '~', '[', '@', ';', ' ', '\t'];
    let cut = trimmed
        .find(|c: char| cut_chars.contains(&c))
        .unwrap_or(trimmed.len());
    let bare = trimmed[..cut].trim();
    if bare.is_empty() {
        return None;
    }
    // Whitespace-normalize so `pkg>=1.0` and `pkg >= 1.0` compare
    // equal. Strips internal whitespace too — `; sys_platform == 'x'`
    // and `;sys_platform=='x'` both collapse to the same canonical
    // form, which makes byte-equal comparison actually useful in
    // practice.
    let tail: String = trimmed[cut..]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    Some((bare, tail))
}

/// Check whether an inline dep must bypass pool reuse regardless of
/// what the pool has.
///
/// Three constructs we refuse:
/// - Exact pin (`pkg==X.Y.Z`): pool's installed version is driven by
///   user settings and may differ. Running against a wrong-version
///   pool env would silently break user code.
/// - Extras (`pkg[feature]`): the extra pulls in transitive deps the
///   pool may not have installed. Can't verify from the spec string.
/// - Direct reference (`pkg @ https://...`, `pkg @ git+...`): pool
///   has the registry version, notebook asked for a specific source.
///   Different sources, not interchangeable.
fn inline_dep_forbids_pool_reuse(dep: &str) -> bool {
    // Strip env marker first so `gremlin ; sys_platform == 'darwin'`
    // isn't mistaken for an exact pin on the gremlin package.
    let before_marker = dep.split(';').next().unwrap_or(dep).trim();
    before_marker.contains("==") || before_marker.contains('[') || before_marker.contains('@')
}

/// Extract the package name from a conda dependency specifier, stripping
/// channel qualifiers (`conda-forge::numpy`) and version constraints
/// (`numpy>=1.24`).  Returns the bare, untrimmed name suitable for
/// [`normalize_package_name`].
///
/// Examples:
/// - `"numpy"` → `Some("numpy")`
/// - `"numpy>=1.24"` → `Some("numpy")`
/// - `"conda-forge::numpy>=1.24"` → `Some("numpy")`
/// - `"conda-forge::numpy"` → `Some("numpy")`
/// - `""` → `None`
fn extract_conda_package_name(dep: &str) -> Option<&str> {
    let trimmed = dep.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Strip channel qualifier (e.g. "conda-forge::numpy" → "numpy")
    let after_channel = match trimmed.find("::") {
        Some(pos) => &trimmed[pos + 2..],
        None => trimmed,
    };
    // Strip version/specifier suffix
    let specifier_chars = ['>', '<', '=', '!', '~', '[', ';', '@'];
    let name = match after_channel.find(|c: char| specifier_chars.contains(&c) || c.is_whitespace())
    {
        Some(pos) => &after_channel[..pos],
        None => after_channel,
    };
    let name = name.trim();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Normalize a package name for comparison: lowercase, replace `_` with `-`.
fn normalize_package_name(name: &str) -> String {
    name.to_lowercase().replace('_', "-")
}

/// Compare inline deps against pool prewarmed packages.
///
/// Matching rule:
/// - Bare name on both sides must be equal (after lowercase + `_`→`-`).
/// - If the inline dep has no constraint tail, any pool entry on the
///   same bare name covers it — the notebook accepts whatever version
///   the pool has installed.
/// - If the inline dep has a constraint tail, the pool must carry the
///   *byte-equal* tail on the same bare name. This is the common case:
///   a new notebook seeded from user settings and the pool env built
///   from those same settings both carry `numpy>=2.2.6` verbatim.
///
/// When bare names match but constraint tails differ
/// (`inline: numpy<2`, `pool: numpy>=2.2.6`), we return
/// [`PoolDepRelation::Independent`] — the pool has the wrong version
/// range and reusing it would silently launch with a version the
/// notebook didn't ask for. We don't attempt to solve version
/// ranges; that's the package manager's job on a full build.
///
/// Forbidden constructs (exact pins, extras, direct references) also
/// force Independent via [`inline_dep_forbids_pool_reuse`].
///
/// [`PoolDepRelation::Additive`] is reserved for the case where a dep
/// is missing from the pool *entirely* (different bare name). The
/// caller can install that missing dep on top of the pool env.
pub fn compare_deps_to_pool(inline_deps: &[String], pool_packages: &[String]) -> PoolDepRelation {
    if inline_deps.is_empty() {
        return PoolDepRelation::Subset;
    }

    // Pool map: bare_name -> set of (normalized) constraint tails
    // observed on that name. A bare unconditional pool entry
    // contributes "" to the set; a marker-gated or version-spec'd
    // entry contributes its canonical tail.
    let mut pool_map: HashMap<String, HashSet<String>> = HashMap::new();
    for p in pool_packages {
        if let Some((bare, tail)) = split_bare_and_constraint(p) {
            pool_map
                .entry(normalize_package_name(bare))
                .or_default()
                .insert(tail);
        }
    }

    let mut delta = Vec::new();

    for dep in inline_deps {
        if inline_dep_forbids_pool_reuse(dep) {
            return PoolDepRelation::Independent;
        }
        let Some((bare, tail)) = split_bare_and_constraint(dep) else {
            // Empty / whitespace-only dep — nothing to match.
            continue;
        };

        let key = normalize_package_name(bare);
        match pool_map.get(&key) {
            None => {
                // Pool doesn't have this package at all — can be
                // installed additively on top of the pool env.
                delta.push(dep.clone());
            }
            Some(pool_tails) if pool_tails.contains(&tail) => {
                // Same canonical tail on both sides (including the
                // unconditional-both case where tails are empty).
                // Covered.
            }
            Some(_) => {
                // Pool has the bare name but with a different
                // constraint or marker. Three flavors all unsafe:
                //  - Pool pinned to a range, inline wants a different
                //    range (or bare) — can't verify pool's installed
                //    version satisfies the request.
                //  - Pool marker-gated, inline unconditional — pool
                //    env may not actually have the package installed
                //    on platforms where the marker evaluated false.
                //  - Inline marker-gated, pool unconditional — same
                //    risk in reverse; we don't evaluate markers here.
                // Force Independent so the full-build path resolves
                // everything from scratch.
                return PoolDepRelation::Independent;
            }
        }
    }

    if delta.is_empty() {
        PoolDepRelation::Subset
    } else {
        PoolDepRelation::Additive { delta }
    }
}

/// Check if a cached UV inline environment already exists for the given deps.
///
/// Returns `Some(PreparedEnv)` on cache hit, `None` on miss.
///
/// On hit, when `bootstrap_dx` is on, re-vendor the launcher into the
/// cached venv before returning. Reason: since 0.2.0,
/// `inline_deps_with_bootstrap` is a no-op and the env hash no longer
/// differs by feature flag, so a pre-upgrade non-bootstrap cache entry
/// (or a pre-0.2.0 single-file-launcher bootstrap entry) can answer a
/// bootstrap launch. `vendor_into_venv` is idempotent + cleans up the
/// legacy single-file module, so calling it unconditionally on hit
/// brings the cached env up to today's layout before the kernel boots.
pub async fn check_uv_inline_cache(
    deps: &[String],
    prerelease: Option<&str>,
    bootstrap_dx: bool,
) -> Option<PreparedEnv> {
    let uv_deps = kernel_env::UvDependencies {
        dependencies: inline_deps_with_bootstrap(deps, bootstrap_dx),
        requires_python: Some(">=3.13".to_string()),
        prerelease: prerelease.map(|s| s.to_string()),
    };

    let hash = kernel_env::uv::compute_env_hash(&uv_deps, None);
    let cache_dir = inline_cache_dir();
    let venv_path = cache_dir.join(&hash);

    #[cfg(unix)]
    let python_path = venv_path.join("bin").join("python");
    #[cfg(windows)]
    let python_path = venv_path.join("Scripts").join("python.exe");

    if !python_path.exists() {
        return None;
    }

    if bootstrap_dx {
        if let Err(err) = kernel_env::launcher::vendor_into_venv(&python_path).await {
            tracing::warn!(
                "[inline-env] UV cache hit at {:?}: vendor_into_venv failed: {}",
                python_path,
                err
            );
        }
    }

    Some(PreparedEnv {
        env_path: venv_path,
        python_path,
    })
}

/// Check if a cached Conda inline environment already exists for the given deps.
///
/// Returns `Some(PreparedEnv)` on cache hit, `None` on miss.
///
/// Beyond checking that the python binary exists, this also verifies that
/// every requested package has a corresponding `conda-meta/` record.  A
/// stale cache entry (e.g. created by a buggy build that dropped packages)
/// is treated as a miss and removed so the next code path can rebuild it.
pub fn check_conda_inline_cache(deps: &[String], channels: &[String]) -> Option<PreparedEnv> {
    let conda_deps = kernel_env::CondaDependencies {
        dependencies: deps.to_vec(),
        channels: if channels.is_empty() {
            vec!["conda-forge".to_string()]
        } else {
            channels.to_vec()
        },
        python: None,
        env_id: None,
    };

    let hash = kernel_env::conda::compute_env_hash(&conda_deps);
    let cache_dir = inline_cache_dir();
    let env_path = cache_dir.join(&hash);

    #[cfg(unix)]
    let python_path = env_path.join("bin").join("python");
    #[cfg(windows)]
    let python_path = env_path.join("Scripts").join("python.exe");

    if !python_path.exists() {
        return None;
    }

    // Verify that every requested package is actually installed.  The
    // python binary existing is necessary but not sufficient — a prior
    // buggy build may have cached an env missing some packages (#2137).
    if !deps.is_empty() {
        let installed = conda_meta_package_names(&env_path);
        for dep in deps {
            let Some(name) = extract_conda_package_name(dep) else {
                continue;
            };
            if !installed.contains(&normalize_package_name(name)) {
                tracing::warn!(
                    "[inline-env] Conda cache {:?} missing requested package {:?} — evicting stale cache",
                    env_path, dep
                );
                let _ = std::fs::remove_dir_all(&env_path);
                return None;
            }
        }
    }

    Some(PreparedEnv {
        env_path,
        python_path,
    })
}

/// Read the `conda-meta/` directory and return a set of installed package
/// names (normalized: lowercase, underscores replaced with hyphens).
///
/// Conda-meta filenames follow `{name}-{version}-{build}.json`.  We parse
/// the name by splitting on `-` and taking the longest prefix whose next
/// segment starts with a digit (the version).  This handles names with
/// hyphens like `scikit-learn-1.4.0-py312_0.json`.
fn conda_meta_package_names(env_path: &std::path::Path) -> HashSet<String> {
    let meta_dir = env_path.join("conda-meta");
    let mut names = HashSet::new();

    let entries = match std::fs::read_dir(&meta_dir) {
        Ok(e) => e,
        Err(_) => return names,
    };

    for entry in entries.flatten() {
        let fname = entry.file_name();
        let fname = fname.to_string_lossy();
        // Skip non-json and the `history` file
        let Some(stem) = fname.strip_suffix(".json") else {
            continue;
        };

        // Find the package name: take segments before the first segment
        // that looks like a version number (starts with a digit).
        let segments: Vec<&str> = stem.split('-').collect();
        let mut name_end = 0;
        for (i, seg) in segments.iter().enumerate() {
            if i > 0 && seg.starts_with(|c: char| c.is_ascii_digit()) {
                break;
            }
            name_end = i + 1;
        }
        if name_end > 0 {
            let pkg_name = segments[..name_end].join("-");
            names.insert(normalize_package_name(&pkg_name));
        }
    }

    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime_doc::RuntimeStateDoc;

    fn runtime_state_handle() -> RuntimeStateHandle {
        let (changed_tx, _) = tokio::sync::broadcast::channel(8);
        RuntimeStateHandle::new(RuntimeStateDoc::new(), changed_tx)
    }

    #[test]
    fn broadcast_progress_handler_writes_state_from_serialized_phase() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        let state = runtime_state_handle();
        let handler = BroadcastProgressHandler::with_state(tx, state.clone());

        handler.on_progress("uv", EnvProgressPhase::OfflineHit);

        let progress = state
            .read(|sd| sd.read_state().env.progress)
            .expect("read runtime state");
        assert_eq!(
            progress,
            Some(serde_json::json!({
                "env_type": "uv",
                "phase": "offline_hit",
            }))
        );

        let broadcast = rx.try_recv().expect("progress broadcast");
        assert!(matches!(
            broadcast,
            NotebookBroadcast::EnvProgress {
                env_type,
                phase: EnvProgressPhase::OfflineHit,
            } if env_type == "uv"
        ));
    }

    #[test]
    fn broadcast_progress_handler_throttles_high_frequency_crdt_writes_only() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        let state = runtime_state_handle();
        let handler = BroadcastProgressHandler::with_state(tx, state.clone());

        handler.on_progress(
            "conda",
            EnvProgressPhase::DownloadProgress {
                completed: 1,
                total: 10,
                current_package: "numpy".to_string(),
                bytes_downloaded: 100,
                bytes_total: Some(1000),
                bytes_per_second: 50.0,
            },
        );
        handler.on_progress(
            "conda",
            EnvProgressPhase::DownloadProgress {
                completed: 2,
                total: 10,
                current_package: "numpy".to_string(),
                bytes_downloaded: 200,
                bytes_total: Some(1000),
                bytes_per_second: 60.0,
            },
        );

        let progress = state
            .read(|sd| sd.read_state().env.progress)
            .expect("read runtime state");
        assert_eq!(
            progress,
            Some(serde_json::json!({
                "env_type": "conda",
                "phase": "download_progress",
                "completed": 1,
                "total": 10,
                "current_package": "numpy",
                "bytes_downloaded": 100,
                "bytes_total": 1000,
                "bytes_per_second": 50.0,
            }))
        );

        assert!(matches!(
            rx.try_recv().expect("first broadcast"),
            NotebookBroadcast::EnvProgress { .. }
        ));
        assert!(matches!(
            rx.try_recv().expect("second broadcast"),
            NotebookBroadcast::EnvProgress { .. }
        ));

        handler.on_progress(
            "conda",
            EnvProgressPhase::Ready {
                env_path: "/tmp/env".to_string(),
                python_path: "/tmp/env/bin/python".to_string(),
            },
        );

        let progress = state
            .read(|sd| sd.read_state().env.progress)
            .expect("read runtime state");
        assert_eq!(
            progress,
            Some(serde_json::json!({
                "env_type": "conda",
                "phase": "ready",
                "env_path": "/tmp/env",
                "python_path": "/tmp/env/bin/python",
            }))
        );
    }

    #[test]
    fn broadcast_progress_handler_without_state_still_broadcasts() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        let handler = BroadcastProgressHandler::new(tx);

        handler.on_progress("pixi", EnvProgressPhase::OfflineHit);

        let broadcast = rx.try_recv().expect("progress broadcast");
        assert!(matches!(
            broadcast,
            NotebookBroadcast::EnvProgress {
                env_type,
                phase: EnvProgressPhase::OfflineHit,
            } if env_type == "pixi"
        ));
    }

    // Helper: take the bare name returned by split_bare_and_constraint.
    fn bare_of(dep: &str) -> Option<String> {
        split_bare_and_constraint(dep).map(|(b, _)| b.to_string())
    }

    // Helper: take the normalized tail returned by split_bare_and_constraint.
    fn tail_of(dep: &str) -> Option<String> {
        split_bare_and_constraint(dep).map(|(_, t)| t)
    }

    #[test]
    fn test_split_bare_and_constraint_bare_names() {
        assert_eq!(bare_of("pandas").as_deref(), Some("pandas"));
        assert_eq!(bare_of("numpy").as_deref(), Some("numpy"));
        assert_eq!(bare_of("  pandas  ").as_deref(), Some("pandas"));
        assert_eq!(bare_of(""), None);
        assert_eq!(tail_of("pandas").as_deref(), Some(""));
    }

    #[test]
    fn test_split_bare_and_constraint_version_specifiers() {
        // Version specifiers get split out as the tail — bare name
        // stays clean.
        assert_eq!(bare_of("pandas>=2.0").as_deref(), Some("pandas"));
        assert_eq!(tail_of("pandas>=2.0").as_deref(), Some(">=2.0"));
        assert_eq!(bare_of("pandas==2.0.0").as_deref(), Some("pandas"));
        assert_eq!(tail_of("pandas==2.0.0").as_deref(), Some("==2.0.0"));
        // Whitespace around the specifier normalizes away so
        // `pkg>=1.0` and `pkg >= 1.0` compare equal.
        assert_eq!(tail_of("pandas >= 2.0").as_deref(), Some(">=2.0"));
    }

    #[test]
    fn test_split_bare_and_constraint_keeps_marker() {
        // Markers are load-bearing: a pool entry with
        // `pkg ; sys_platform == 'darwin'` only installs on Darwin, so
        // the marker MUST survive into the tail. Comparison against
        // bare-`pkg` must fail on platforms where the marker evaluated
        // false (pool advertises the spec but package isn't installed).
        assert_eq!(
            bare_of("gremlin ; sys_platform == 'darwin'").as_deref(),
            Some("gremlin")
        );
        // Canonical tail: whitespace stripped.
        assert_eq!(
            tail_of("gremlin ; sys_platform == 'darwin'").as_deref(),
            Some(";sys_platform=='darwin'")
        );
        // Versioned + marker: tail carries both.
        assert_eq!(
            tail_of("pandas >= 2.0 ; python_version >= '3.10'").as_deref(),
            Some(">=2.0;python_version>='3.10'")
        );
    }

    #[test]
    fn test_inline_dep_forbids_pool_reuse() {
        // Exact pins and extras force Independent.
        assert!(inline_dep_forbids_pool_reuse("pandas==2.0.0"));
        assert!(inline_dep_forbids_pool_reuse("pandas[sql]"));
        assert!(inline_dep_forbids_pool_reuse("pandas[sql]>=2.0"));

        // Non-exact specifiers are safe for pool matching.
        assert!(!inline_dep_forbids_pool_reuse("pandas"));
        assert!(!inline_dep_forbids_pool_reuse("pandas>=2.0"));
        assert!(!inline_dep_forbids_pool_reuse("pandas<3"));
        assert!(!inline_dep_forbids_pool_reuse("pandas~=2.0"));

        // `==` inside an environment marker is NOT a pin.
        assert!(!inline_dep_forbids_pool_reuse(
            "gremlin ; sys_platform == 'darwin'"
        ));
    }

    #[test]
    fn test_extract_conda_package_name() {
        // Bare names
        assert_eq!(extract_conda_package_name("numpy"), Some("numpy"));
        assert_eq!(extract_conda_package_name("pandas"), Some("pandas"));
        assert_eq!(extract_conda_package_name("  scipy  "), Some("scipy"));
        assert_eq!(extract_conda_package_name(""), None);

        // Version specifiers
        assert_eq!(extract_conda_package_name("numpy>=1.24"), Some("numpy"));
        assert_eq!(extract_conda_package_name("pandas==2.0.0"), Some("pandas"));
        assert_eq!(extract_conda_package_name("pandas<3"), Some("pandas"));
        assert_eq!(extract_conda_package_name("pandas~=2.0"), Some("pandas"));

        // Channel qualifiers
        assert_eq!(
            extract_conda_package_name("conda-forge::numpy"),
            Some("numpy")
        );
        assert_eq!(
            extract_conda_package_name("conda-forge::numpy>=1.24"),
            Some("numpy")
        );
        assert_eq!(extract_conda_package_name("defaults::scipy"), Some("scipy"));

        // Extras / markers
        assert_eq!(extract_conda_package_name("pandas[sql]"), Some("pandas"));
        assert_eq!(
            extract_conda_package_name("pandas ; python_version >= '3.8'"),
            Some("pandas")
        );
    }

    #[test]
    fn test_normalize_package_name() {
        assert_eq!(normalize_package_name("Pandas"), "pandas");
        assert_eq!(normalize_package_name("scikit_learn"), "scikit-learn");
        assert_eq!(normalize_package_name("PyArrow"), "pyarrow");
    }

    #[test]
    fn test_compare_subset() {
        let pool = vec![
            "ipykernel".into(),
            "pandas".into(),
            "numpy".into(),
            "matplotlib".into(),
        ];
        let deps = vec!["pandas".into(), "numpy".into()];
        assert!(matches!(
            compare_deps_to_pool(&deps, &pool),
            PoolDepRelation::Subset
        ));
    }

    #[test]
    fn test_compare_subset_case_insensitive() {
        let pool = vec!["ipykernel".into(), "PyArrow".into()];
        let deps = vec!["pyarrow".into()];
        assert!(matches!(
            compare_deps_to_pool(&deps, &pool),
            PoolDepRelation::Subset
        ));
    }

    #[test]
    fn test_compare_additive() {
        let pool = vec!["ipykernel".into(), "pandas".into()];
        let deps = vec!["pandas".into(), "scikit-learn".into()];
        match compare_deps_to_pool(&deps, &pool) {
            PoolDepRelation::Additive { delta } => {
                assert_eq!(delta, vec!["scikit-learn".to_string()]);
            }
            other => panic!("expected Additive, got {:?}", other),
        }
    }

    #[test]
    fn test_compare_independent_version_pin() {
        let pool = vec!["ipykernel".into(), "pandas".into()];
        let deps = vec!["pandas==2.0.0".into()];
        assert!(matches!(
            compare_deps_to_pool(&deps, &pool),
            PoolDepRelation::Independent
        ));
    }

    #[test]
    fn test_compare_empty_deps() {
        let pool = vec!["ipykernel".into()];
        assert!(matches!(
            compare_deps_to_pool(&[], &pool),
            PoolDepRelation::Subset
        ));
    }

    #[test]
    fn test_compare_underscore_normalization() {
        let pool = vec!["scikit-learn".into()];
        let deps = vec!["scikit_learn".into()];
        assert!(matches!(
            compare_deps_to_pool(&deps, &pool),
            PoolDepRelation::Subset
        ));
    }

    #[test]
    fn test_compare_subset_with_version_specifiers() {
        // Both sides carry version specifiers (pool list comes from
        // `user_default_packages` verbatim, same with seeded inline deps).
        // Match on bare name after stripping both.
        let pool = vec![
            "ipykernel".into(),
            "numpy>=2.2.6".into(),
            "pandas".into(),
            "pyarrow>=14".into(),
        ];
        let deps = vec!["numpy>=2.2.6".into(), "pandas".into(), "pyarrow>=14".into()];
        assert!(matches!(
            compare_deps_to_pool(&deps, &pool),
            PoolDepRelation::Subset
        ));
    }

    #[test]
    fn test_compare_subset_matches_real_user_seeded_notebook() {
        // Exact shape of a newly-created notebook seeded from the
        // default rgbkrk user settings (9 UV deps, mix of bare +
        // version-specifier + marker). Pool built from the same set
        // plus the pool-essentials prefix.
        let pool = vec![
            "ipykernel".into(),
            "ipywidgets".into(),
            "anywidget".into(),
            "nbformat".into(),
            "uv".into(),
            "dx".into(),
            "gremlin ; sys_platform == 'darwin'".into(),
            "narwhals>=1.0".into(),
            "nteract".into(),
            "nteract-kernel-launcher".into(),
            "numpy>=2.2.6".into(),
            "pandas".into(),
            "polars".into(),
            "pyarrow>=14".into(),
        ];
        let inline = vec![
            "dx".into(),
            "gremlin ; sys_platform == 'darwin'".into(),
            "narwhals>=1.0".into(),
            "nteract".into(),
            "nteract-kernel-launcher".into(),
            "numpy>=2.2.6".into(),
            "pandas".into(),
            "polars".into(),
            "pyarrow>=14".into(),
        ];
        assert!(matches!(
            compare_deps_to_pool(&inline, &pool),
            PoolDepRelation::Subset
        ));
    }

    #[test]
    fn test_compare_additive_only_for_missing_bare_names() {
        // Additive delta is reserved for deps whose bare name isn't in
        // the pool at all. Constraint-mismatch on a matching bare name
        // goes to Independent instead (no silent wrong-version reuse).
        //
        // Pool has bare `pandas` + missing `torch`, inline says
        // `pandas` (bare, accepts any) + `torch` — only `torch` is
        // delta; `pandas` bare-vs-bare covers.
        let pool = vec!["ipykernel".into(), "pandas".into()];
        let deps = vec!["pandas".into(), "torch".into()];
        match compare_deps_to_pool(&deps, &pool) {
            PoolDepRelation::Additive { delta } => {
                assert_eq!(delta, vec!["torch".to_string()]);
            }
            other => panic!("expected Additive, got {:?}", other),
        }
    }

    #[test]
    fn test_compare_independent_on_extras() {
        // Extras pull in transitive deps the pool may not have.
        let pool = vec!["ipykernel".into(), "pandas".into()];
        let deps = vec!["pandas[parquet]".into()];
        assert!(matches!(
            compare_deps_to_pool(&deps, &pool),
            PoolDepRelation::Independent
        ));
    }

    #[test]
    fn test_compare_independent_on_constraint_mismatch() {
        // Pool was built with `numpy>=2.2.6`. Notebook asks for
        // `numpy<2`. Pool's installed numpy is 2.x-something; running
        // user code against it would silently violate the `<2` bound.
        // Must force fresh build.
        let pool = vec!["ipykernel".into(), "numpy>=2.2.6".into()];
        let deps = vec!["numpy<2".into()];
        assert!(matches!(
            compare_deps_to_pool(&deps, &pool),
            PoolDepRelation::Independent
        ));
    }

    #[test]
    fn test_compare_independent_on_space_separated_conda_constraint() {
        // Conda-style `numpy 1.24.*` (space-separated version spec).
        // Pool has bare `numpy`; inline wants `1.24.*`. Different
        // constraint → Independent.
        let pool = vec!["ipykernel".into(), "numpy".into()];
        let deps = vec!["numpy 1.24.*".into()];
        assert!(matches!(
            compare_deps_to_pool(&deps, &pool),
            PoolDepRelation::Independent
        ));
    }

    #[test]
    fn test_compare_independent_on_direct_reference() {
        // `pkg @ URL` is a direct reference to a specific source. Pool
        // has registry-installed pkg; different source → refuse reuse.
        let pool = vec!["ipykernel".into(), "pandas".into()];
        let deps = vec!["pandas @ https://example.com/pandas.whl".into()];
        assert!(matches!(
            compare_deps_to_pool(&deps, &pool),
            PoolDepRelation::Independent
        ));
    }

    #[test]
    fn test_compare_independent_bare_inline_vs_pool_constrained() {
        // Pool's `prewarmed_packages` is the install-spec list, not
        // what's actually installed. A constrained pool entry might
        // or might not have produced an installed package (marker
        // evaluated false, resolver skipped it). So even a bare
        // inline dep that would "accept any version" can't safely
        // reuse a pool whose entry carries constraints — the package
        // may not be installed at all.
        let pool = vec!["ipykernel".into(), "numpy>=2.2.6".into()];
        let deps = vec!["numpy".into()];
        assert!(matches!(
            compare_deps_to_pool(&deps, &pool),
            PoolDepRelation::Independent
        ));
    }

    #[test]
    fn test_compare_independent_marker_gated_pool_vs_bare_inline() {
        // Codex-flagged regression: `gremlin ; sys_platform == 'darwin'`
        // in the pool's install-spec list only produced an installed
        // `gremlin` on Darwin pool envs. On Linux, the pool env carries
        // the spec in `prewarmed_packages` but the package isn't there.
        // A later bare-`gremlin` inline dep MUST NOT reuse that pool
        // env, or the notebook launches with a missing import.
        let pool = vec![
            "ipykernel".into(),
            "gremlin ; sys_platform == 'darwin'".into(),
        ];
        let deps = vec!["gremlin".into()];
        assert!(matches!(
            compare_deps_to_pool(&deps, &pool),
            PoolDepRelation::Independent
        ));
    }

    #[test]
    fn test_compare_subset_marker_on_both_sides() {
        // When the inline dep and pool spec carry the same marker
        // verbatim (the common case: notebook seeded from settings and
        // pool built from same settings), they canonicalize to the
        // same tail and match.
        let pool = vec![
            "ipykernel".into(),
            "gremlin ; sys_platform == 'darwin'".into(),
        ];
        let deps = vec!["gremlin ; sys_platform == 'darwin'".into()];
        assert!(matches!(
            compare_deps_to_pool(&deps, &pool),
            PoolDepRelation::Subset
        ));
    }

    #[test]
    fn test_compare_subset_normalizes_internal_whitespace() {
        // `pkg>=1.0` (no spaces) and `pkg >= 1.0` (spaces) produce
        // the same canonical tail, so either form on one side
        // matches the other.
        let pool = vec!["ipykernel".into(), "numpy>=2.2.6".into()];
        let deps = vec!["numpy >= 2.2.6".into()];
        assert!(matches!(
            compare_deps_to_pool(&deps, &pool),
            PoolDepRelation::Subset
        ));
    }

    #[test]
    fn test_compare_independent_inline_constrained_pool_bare() {
        // Flip of the prior test: inline has a constraint, pool
        // entry is bare (no constraint recorded). Pool's installed
        // version may or may not satisfy the inline constraint —
        // we can't tell without introspection, so refuse.
        let pool = vec!["ipykernel".into(), "numpy".into()];
        let deps = vec!["numpy>=2.2.6".into()];
        assert!(matches!(
            compare_deps_to_pool(&deps, &pool),
            PoolDepRelation::Independent
        ));
    }

    #[test]
    fn test_conda_meta_package_names_parses_filenames() {
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("conda-meta");
        std::fs::create_dir_all(&meta).unwrap();

        // Standard packages
        std::fs::write(meta.join("numpy-2.4.3-py314h2b28147_0.json"), "{}").unwrap();
        std::fs::write(meta.join("pandas-2.3.0-py314ha1ea8a9_0.json"), "{}").unwrap();
        std::fs::write(meta.join("scipy-1.17.1-py314hf07bd8e_0.json"), "{}").unwrap();
        // Hyphenated package name
        std::fs::write(meta.join("scikit-learn-1.4.0-py312_0.json"), "{}").unwrap();
        // Leading underscore
        std::fs::write(meta.join("_openmp_mutex-4.5-20_gnu.json"), "{}").unwrap();
        // history file (not a package)
        std::fs::write(meta.join("history"), "").unwrap();

        let names = conda_meta_package_names(dir.path());
        assert!(names.contains("numpy"), "missing numpy: {:?}", names);
        assert!(names.contains("pandas"), "missing pandas: {:?}", names);
        assert!(names.contains("scipy"), "missing scipy: {:?}", names);
        assert!(
            names.contains("scikit-learn"),
            "missing scikit-learn: {:?}",
            names
        );
        assert!(
            names.contains("-openmp-mutex"),
            "missing _openmp_mutex: {:?}",
            names
        );
        assert!(
            !names.contains("history"),
            "should not contain history: {:?}",
            names
        );
    }

    #[test]
    fn test_conda_cache_miss_on_missing_package() {
        // This tests the package validation logic in check_conda_inline_cache
        // indirectly through conda_meta_package_names.
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("conda-meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("numpy-2.4.3-py314h2b28147_0.json"), "{}").unwrap();
        std::fs::write(meta.join("scipy-1.17.1-py314hf07bd8e_0.json"), "{}").unwrap();

        let names = conda_meta_package_names(dir.path());
        // pandas is NOT installed
        assert!(names.contains("numpy"));
        assert!(names.contains("scipy"));
        assert!(!names.contains("pandas"), "pandas should not be present");
        assert!(
            !names.contains("matplotlib"),
            "matplotlib should not be present"
        );
    }
}
