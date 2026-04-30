use super::*;
use runtime_doc::{KernelActivity, KernelErrorReason, RuntimeLifecycle};

pub struct TrustState {
    pub status: runt_trust::TrustStatus,
    #[allow(dead_code)]
    pub info: runt_trust::TrustInfo,
    /// If true, kernel launch is pending user trust approval
    #[allow(dead_code)]
    pub pending_launch: bool,
}

fn enrich_trust_info(room: &NotebookRoom, trust: &mut TrustState) {
    if let Err(error) = room.trusted_packages.enrich_info(&mut trust.info) {
        warn!(
            "[trusted-packages] Failed to read package allowlist; continuing without approved markers: {}",
            error
        );
    }
}

fn trust_status_str(status: &runt_trust::TrustStatus) -> &'static str {
    match status {
        runt_trust::TrustStatus::Trusted => "trusted",
        runt_trust::TrustStatus::Untrusted => "untrusted",
        runt_trust::TrustStatus::SignatureInvalid => "signature_invalid",
        runt_trust::TrustStatus::NoDependencies => "no_dependencies",
    }
}

fn trust_needs_approval(status: &runt_trust::TrustStatus) -> bool {
    !matches!(
        status,
        runt_trust::TrustStatus::Trusted | runt_trust::TrustStatus::NoDependencies
    )
}

pub(crate) fn write_trust_to_runtime_state(room: &NotebookRoom, trust: &TrustState) {
    if let Err(e) = room.state.with_doc(|sd| {
        sd.set_trust_with_approved(
            trust_status_str(&trust.status),
            trust_needs_approval(&trust.status),
            &trust.info.approved_uv_dependencies,
            &trust.info.approved_conda_dependencies,
            &trust.info.approved_pixi_dependencies,
            &trust.info.approved_pixi_pypi_dependencies,
        )
    }) {
        warn!("[runtime-state] {}", e);
    }
}

/// Check if a notebook's metadata snapshot has inline dependencies or Deno config.
/// Returns the appropriate env_source if found ("uv:inline", "conda:inline", or "deno").
///
/// Priority: Deno is checked first, then UV deps, then conda deps.
/// Check order: deno > uv > pixi > conda. Unlike detect_manager_from_metadata
/// (which checks section presence with pixi > conda > uv), this function checks
/// for non-empty deps. UV is checked first because it's the most common case
/// and only one section will have deps in practice (build_new_notebook_metadata
/// creates exactly one section).
/// Fallback package manager for the prewarmed pool when neither metadata nor
/// request specify one. Maps the daemon's `default_python_env` setting.
pub(crate) fn default_prewarmed_manager(
    setting: crate::settings_doc::PythonEnvType,
) -> notebook_protocol::connection::PackageManager {
    use notebook_protocol::connection::PackageManager;
    match setting {
        crate::settings_doc::PythonEnvType::Conda => PackageManager::Conda,
        crate::settings_doc::PythonEnvType::Pixi => PackageManager::Pixi,
        _ => PackageManager::Uv,
    }
}

pub(crate) fn check_inline_deps(
    snapshot: &NotebookMetadataSnapshot,
) -> Option<notebook_protocol::connection::EnvSource> {
    use notebook_protocol::connection::{EnvSource, PackageManager};

    if snapshot.runt.deno.is_some() {
        return Some(EnvSource::Deno);
    }

    if let Some(ref uv) = snapshot.runt.uv {
        if !uv.dependencies.is_empty() {
            return Some(EnvSource::Inline(PackageManager::Uv));
        }
    }

    // Check pixi dependencies (conda + pypi)
    if let Some(ref pixi) = snapshot.runt.pixi {
        if !pixi.dependencies.is_empty() {
            return Some(EnvSource::Inline(PackageManager::Pixi));
        }
    }

    // Check conda dependencies
    if let Some(ref conda) = snapshot.runt.conda {
        if !conda.dependencies.is_empty() {
            return Some(EnvSource::Inline(PackageManager::Conda));
        }
    }

    None
}

/// Detect which package manager section exists in the metadata, regardless of
/// whether it has deps. Used to pick the correct prewarmed pool type when
/// check_inline_deps returns None (empty deps, explicit manager).
///
/// Priority: pixi > conda > uv. Pixi is most specific (manages both conda
/// and pypi deps). Matches detect_package_manager in runt-mcp and
/// get_metadata_env_type in runtimed-py.
fn detect_manager_from_metadata(
    snapshot: &NotebookMetadataSnapshot,
) -> Option<notebook_protocol::connection::PackageManager> {
    use notebook_protocol::connection::PackageManager;
    if snapshot.runt.pixi.is_some() {
        Some(PackageManager::Pixi)
    } else if snapshot.runt.conda.is_some() {
        Some(PackageManager::Conda)
    } else if snapshot.runt.uv.is_some() {
        Some(PackageManager::Uv)
    } else {
        None
    }
}

/// Extract inline conda dependencies from a metadata snapshot.
/// Returns the list of dependency strings if conda deps are present.
pub(crate) fn get_inline_conda_deps(snapshot: &NotebookMetadataSnapshot) -> Option<Vec<String>> {
    if let Some(ref conda) = snapshot.runt.conda {
        if !conda.dependencies.is_empty() {
            return Some(conda.dependencies.clone());
        }
    }
    None
}

/// Extract inline UV dependencies from a metadata snapshot.
/// Returns the list of dependency strings if UV deps are present.
pub(crate) fn get_inline_uv_deps(snapshot: &NotebookMetadataSnapshot) -> Option<Vec<String>> {
    if let Some(ref uv) = snapshot.runt.uv {
        if !uv.dependencies.is_empty() {
            return Some(uv.dependencies.clone());
        }
    }
    None
}

/// Extract UV prerelease strategy from a metadata snapshot.
pub(crate) fn get_inline_uv_prerelease(snapshot: &NotebookMetadataSnapshot) -> Option<String> {
    snapshot
        .runt
        .uv
        .as_ref()
        .and_then(|uv| uv.prerelease.clone())
}

/// Extract conda channels from a metadata snapshot.
/// Returns the list of channel strings, or defaults to ["conda-forge"].
pub(crate) fn get_inline_conda_channels(snapshot: &NotebookMetadataSnapshot) -> Vec<String> {
    if let Some(ref conda) = snapshot.runt.conda {
        if !conda.channels.is_empty() {
            return conda.channels.clone();
        }
    }
    vec!["conda-forge".to_string()]
}

/// Extract dependency entries from a pixi.toml or pyproject.toml with [tool.pixi].
///
/// Section-aware: only collects `key = value` lines from `[dependencies]`,
/// `[pypi-dependencies]`, `[tool.pixi.dependencies]`, `[tool.pixi.pypi-dependencies]`.
/// Stores the full line (trimmed) so version constraint changes are detected.
pub(crate) fn extract_pixi_toml_deps(content: &str) -> Vec<String> {
    let dep_sections = [
        "[dependencies]",
        "[pypi-dependencies]",
        "[tool.pixi.dependencies]",
        "[tool.pixi.pypi-dependencies]",
    ];
    let mut in_dep_section = false;
    let mut deps = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_dep_section = dep_sections.iter().any(|s| trimmed.eq_ignore_ascii_case(s));
            continue;
        }
        if in_dep_section
            && !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && trimmed.contains('=')
        {
            deps.push(trimmed.to_string());
        }
    }
    deps.sort();
    deps
}

/// Extract dependency strings from a pyproject.toml's `[project].dependencies` list.
/// Returns PEP 508 dependency strings (e.g., "pandas>=2.0", "numpy").
///
/// Only matches `dependencies` when inside the `[project]` table. Resets on
/// any other `[...]` header so deps from `[tool.*]` tables are not captured.
fn extract_pyproject_deps(content: &str) -> Vec<String> {
    let mut in_project = false;
    let mut in_deps = false;
    let mut deps = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        // Track which TOML table we're inside
        if trimmed.starts_with('[') {
            in_deps = false;
            in_project = trimmed == "[project]";
            continue;
        }
        if !in_project {
            continue;
        }
        if trimmed == "dependencies = [" || trimmed.starts_with("dependencies = [") {
            in_deps = true;
            // Handle single-line: dependencies = ["foo", "bar"]
            if let Some(rest) = trimmed.strip_prefix("dependencies = [") {
                if let Some(inner) = rest.strip_suffix(']') {
                    for dep in inner.split(',') {
                        let dep = dep.trim().trim_matches('"').trim_matches('\'').trim();
                        if !dep.is_empty() {
                            deps.push(dep.to_string());
                        }
                    }
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
            let dep = trimmed
                .trim_matches(',')
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .trim();
            if !dep.is_empty() && !dep.starts_with('#') {
                deps.push(dep.to_string());
            }
        }
    }
    deps.sort();
    deps
}

/// Build a LaunchedEnvConfig from the current metadata snapshot.
/// This captures what configuration was used at kernel launch time.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_launched_config(
    kernel_type: &str,
    env_source: &str,
    inline_deps: Option<&[String]>,
    metadata_snapshot: Option<&NotebookMetadataSnapshot>,
    venv_path: Option<PathBuf>,
    python_path: Option<PathBuf>,
    prewarmed_packages: Option<&[String]>,
    notebook_path: Option<&std::path::Path>,
    feature_flags: notebook_protocol::protocol::FeatureFlags,
    captured_env: Option<&CapturedEnv>,
) -> LaunchedEnvConfig {
    let mut config = LaunchedEnvConfig {
        feature_flags,
        ..LaunchedEnvConfig::default()
    };

    match env_source {
        "uv:inline" | "uv:pep723" => {
            config.uv_deps = inline_deps.map(|d| d.to_vec());
            config.venv_path = venv_path;
            config.python_path = python_path;
            if let Some(pkgs) = prewarmed_packages {
                config.prewarmed_packages = pkgs.to_vec();
            }
        }
        "conda:inline" => {
            config.conda_deps = inline_deps.map(|d| d.to_vec());
            config.venv_path = venv_path;
            config.python_path = python_path;
            if let Some(snapshot) = metadata_snapshot {
                config.conda_channels = Some(get_inline_conda_channels(snapshot));
            }
            if let Some(pkgs) = prewarmed_packages {
                config.prewarmed_packages = pkgs.to_vec();
            }
        }
        "uv:prewarmed" => {
            // Store paths so hot-sync can install deps into the prewarmed venv.
            //
            // If this launch routed through the captured-env path (notebook
            // has env_id + captured deps + matching unified-hash env on
            // disk), record the captured deps as the launch baseline. That
            // way `check_and_broadcast_sync_state` sees
            // `is_tracking = true` and reports `in_sync = true` when the
            // metadata still matches what's installed, instead of treating
            // every captured dep as a pending addition on reopen.
            //
            // Otherwise (genuine first-time prewarmed launch, pre-capture
            // notebook) leave `uv_deps = None` to fall through to the
            // "inline deps added post-launch" branch.
            if let Some(CapturedEnv::Uv { deps, .. }) = captured_env {
                config.uv_deps = Some(deps.dependencies.clone());
            }
            config.venv_path = venv_path;
            config.python_path = python_path;
            if let Some(pkgs) = prewarmed_packages {
                config.prewarmed_packages = pkgs.to_vec();
            }
        }
        "conda:prewarmed" => {
            // See `uv:prewarmed` above — same captured-env baseline logic
            // so drift detection works on conda reopens too. Captured conda
            // channels go into `conda_channels` so channel edits are
            // flagged as drift rather than silently ignored.
            if let Some(CapturedEnv::Conda { deps, .. }) = captured_env {
                config.conda_deps = Some(deps.dependencies.clone());
                config.conda_channels = Some(deps.channels.clone());
            }
            config.venv_path = venv_path;
            config.python_path = python_path;
            if let Some(pkgs) = prewarmed_packages {
                config.prewarmed_packages = pkgs.to_vec();
            }
        }
        "uv:pyproject" => {
            // Store pyproject.toml path and env paths for project-aware dep management.
            config.venv_path = venv_path;
            config.python_path = python_path;
            if let Some(nb_path) = notebook_path {
                if let Some(detected) = crate::project_file::detect_project_file(nb_path) {
                    if detected.kind == crate::project_file::ProjectFileKind::PyprojectToml {
                        config.pyproject_path = Some(detected.path);
                    }
                }
            }
        }
        "pixi:toml" => {
            // Store pixi.toml deps baseline for drift detection.
            // Parse section-aware: only collect entries from [dependencies],
            // [pypi-dependencies], [tool.pixi.dependencies], [tool.pixi.pypi-dependencies].
            if let Some(nb_path) = notebook_path {
                if let Some(detected) = crate::project_file::detect_project_file(nb_path) {
                    if detected.kind == crate::project_file::ProjectFileKind::PixiToml {
                        if let Ok(content) = std::fs::read_to_string(&detected.path) {
                            let deps = extract_pixi_toml_deps(&content);
                            config.pixi_toml_deps = Some(deps);
                            config.pixi_toml_path = Some(detected.path);
                        }
                    }
                }
            }
        }
        "conda:env_yml" => {
            // Store environment.yml path and deps baseline for drift detection.
            // CRDT-only conda deps go into conda_deps for sync diff tracking.
            config.conda_deps = metadata_snapshot.and_then(get_inline_conda_deps);
            config.conda_channels = metadata_snapshot.map(get_inline_conda_channels);
            // Pass venv/python paths so runtime agent can reconstruct PooledEnv
            config.venv_path = venv_path;
            config.python_path = python_path;
            if let Some(pkgs) = prewarmed_packages {
                config.prewarmed_packages = pkgs.to_vec();
            }
            if let Some(nb_path) = notebook_path {
                if let Some(detected) = crate::project_file::find_nearest_project_file(
                    nb_path,
                    &[crate::project_file::ProjectFileKind::EnvironmentYml],
                ) {
                    // Parse environment.yml to snapshot deps at launch time
                    if let Ok(env_config) =
                        crate::project_file::parse_environment_yml(&detected.path)
                    {
                        let mut deps = env_config.dependencies.clone();
                        deps.sort();
                        config.environment_yml_deps = Some(deps);
                    }
                    config.environment_yml_path = Some(detected.path);
                }
            }
        }
        "pixi:inline" => {
            // Store pixi deps for drift detection
            config.pixi_deps = inline_deps.map(|d| d.to_vec());
        }
        "pixi:pep723" => {
            // PEP 723 deps come from cell source, not runt.pixi metadata.
            // Don't store in pixi_deps — there's no metadata to diff against.
        }
        "pixi:prewarmed" => {
            // Pixi prewarmed uses pooled env — store paths
            config.venv_path = venv_path;
            config.python_path = python_path;
            if let Some(pkgs) = prewarmed_packages {
                config.prewarmed_packages = pkgs.to_vec();
            }
        }
        _ => {
            // All other Python env sources use pooled environments
            // — store paths so the runtime agent can reconstruct.
            config.venv_path = venv_path;
            config.python_path = python_path;
            if let Some(pkgs) = prewarmed_packages {
                config.prewarmed_packages = pkgs.to_vec();
            }
        }
    }

    // For Deno kernels, capture the deno config
    if kernel_type == "deno" {
        if let Some(snapshot) = metadata_snapshot {
            if let Some(ref deno) = snapshot.runt.deno {
                config.deno_config = Some(DenoLaunchedConfig {
                    permissions: deno.permissions.clone(),
                    import_map: deno.import_map.clone(),
                    config: deno.config.clone(),
                    flexible_npm_imports: deno.flexible_npm_imports.unwrap_or(true),
                });
            }
        }
    }

    // Generate unique launch ID for this kernel session (for race detection during hot-sync)
    config.launch_id = Some(uuid::Uuid::new_v4().to_string());

    config
}

/// Compute the difference between launched config and current metadata.
/// Returns Some(diff) if there are differences, None if in sync.
pub(crate) fn compute_env_sync_diff(
    launched: &LaunchedEnvConfig,
    current: &NotebookMetadataSnapshot,
) -> Option<EnvSyncDiff> {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut deno_changed = false;

    // Check UV deps
    if let Some(ref launched_uv) = launched.uv_deps {
        let current_uv = current
            .runt
            .uv
            .as_ref()
            .map(|u| &u.dependencies[..])
            .unwrap_or(&[]);

        for dep in current_uv {
            if !launched_uv.contains(dep) {
                added.push(dep.clone());
            }
        }
        for dep in launched_uv {
            if !current_uv.contains(dep) {
                removed.push(dep.clone());
            }
        }
    }

    // Check conda deps and channels
    let mut channels_changed = false;
    if let Some(ref launched_conda) = launched.conda_deps {
        let current_conda = current
            .runt
            .conda
            .as_ref()
            .map(|c| &c.dependencies[..])
            .unwrap_or(&[]);

        for dep in current_conda {
            if !launched_conda.contains(dep) {
                added.push(dep.clone());
            }
        }
        for dep in launched_conda {
            if !current_conda.contains(dep) {
                removed.push(dep.clone());
            }
        }

        // Check channels
        if let Some(ref launched_channels) = launched.conda_channels {
            let current_channels = current
                .runt
                .conda
                .as_ref()
                .map(|c| &c.channels[..])
                .unwrap_or(&[]);

            // Channels are ordered, so compare as slices
            if launched_channels.as_slice() != current_channels {
                channels_changed = true;
            }
        }
    }

    // Check pixi deps
    if let Some(ref launched_pixi) = launched.pixi_deps {
        let current_pixi = current
            .runt
            .pixi
            .as_ref()
            .map(|p| &p.dependencies[..])
            .unwrap_or(&[]);

        for dep in current_pixi {
            if !launched_pixi.contains(dep) {
                added.push(dep.clone());
            }
        }
        for dep in launched_pixi {
            if !current_pixi.contains(dep) {
                removed.push(dep.clone());
            }
        }
    }

    // Check pixi:toml deps (file-based drift)
    if let Some(ref launched_toml_deps) = launched.pixi_toml_deps {
        if let Some(ref toml_path) = launched.pixi_toml_path {
            if let Ok(content) = std::fs::read_to_string(toml_path) {
                let current_deps = extract_pixi_toml_deps(&content);

                for dep in &current_deps {
                    if !launched_toml_deps.contains(dep) {
                        // Extract just the package name for the UI
                        let name = dep
                            .split('=')
                            .next()
                            .map(|n| n.trim().to_string())
                            .unwrap_or_else(|| dep.clone());
                        added.push(name);
                    }
                }
                for dep in launched_toml_deps {
                    if !current_deps.contains(dep) {
                        let name = dep
                            .split('=')
                            .next()
                            .map(|n| n.trim().to_string())
                            .unwrap_or_else(|| dep.clone());
                        removed.push(name);
                    }
                }
            }
        }
    }

    // Check conda:env_yml deps (file-based drift)
    if let Some(ref launched_yml_deps) = launched.environment_yml_deps {
        if let Some(ref yml_path) = launched.environment_yml_path {
            if let Ok(env_config) = crate::project_file::parse_environment_yml(yml_path) {
                let mut current_deps = env_config.dependencies;
                current_deps.sort();

                for dep in &current_deps {
                    if !launched_yml_deps.contains(dep) {
                        let name = notebook_doc::metadata::extract_package_name(dep).to_string();
                        added.push(name);
                    }
                }
                for dep in launched_yml_deps {
                    if !current_deps.contains(dep) {
                        let name = notebook_doc::metadata::extract_package_name(dep).to_string();
                        removed.push(name);
                    }
                }
            }
        }
    }

    // Check deno config
    if let Some(ref launched_deno) = launched.deno_config {
        if let Some(ref current_deno) = current.runt.deno {
            let current_flexible = current_deno.flexible_npm_imports.unwrap_or(true);
            if launched_deno.permissions != current_deno.permissions
                || launched_deno.import_map != current_deno.import_map
                || launched_deno.config != current_deno.config
                || launched_deno.flexible_npm_imports != current_flexible
            {
                deno_changed = true;
            }
        } else {
            // Deno config was removed
            deno_changed = true;
        }
    }

    if added.is_empty() && removed.is_empty() && !channels_changed && !deno_changed {
        None
    } else {
        Some(EnvSyncDiff {
            added,
            removed,
            channels_changed,
            deno_changed,
        })
    }
}

/// Rebuild resolved markdown asset refs for all markdown cells.
///
/// This resolves both notebook-relative files and nbformat attachments into
/// blob-store hashes, then updates the cell-local `resolved_assets` maps so
/// isolated markdown rendering can rewrite those refs to blob URLs.
pub(crate) async fn process_markdown_assets(room: &NotebookRoom) {
    let notebook_path = room
        .identity
        .path
        .read()
        .await
        .clone()
        .filter(|p| p.exists());
    let nbformat_attachments = room.nbformat_attachments_snapshot().await;

    // Fork BEFORE async resolution so the fork's baseline predates
    // any concurrent edits. Asset updates on the fork are treated as
    // concurrent with user edits via Automerge's CRDT merge.
    let (markdown_cells, mut fork) = {
        let mut doc = room.doc.write().await;
        let cells: Vec<(String, String, HashMap<String, String>)> = doc
            .get_cells()
            .into_iter()
            .filter(|cell| cell.cell_type == "markdown")
            .map(|cell| (cell.id, cell.source, cell.resolved_assets))
            .collect();
        let fork = doc.fork_with_actor(format!("runtimed:assets:{}", uuid::Uuid::new_v4()));
        (cells, fork)
    };

    let mut any_changed = false;
    for (cell_id, source, existing_assets) in markdown_cells {
        let desired_assets = resolve_markdown_assets(
            &source,
            notebook_path.as_deref(),
            nbformat_attachments.get(&cell_id),
            &room.blob_store,
        )
        .await;

        if desired_assets != existing_assets {
            if let Err(e) = fork.set_cell_resolved_assets(&cell_id, &desired_assets) {
                warn!(
                    "[notebook-sync] Failed to sync resolved markdown assets for {}: {}",
                    cell_id, e
                );
            }
            any_changed = true;
        }
    }

    if !any_changed {
        return;
    }

    let persist_bytes = {
        let mut doc = room.doc.write().await;
        if let Err(e) = catch_automerge_panic("metadata-merge", || doc.merge(&mut fork)) {
            warn!("{}", e);
            doc.rebuild_from_save();
        }
        doc.save()
    };

    let _ = room.broadcasts.changed_tx.send(());
    if let Some(ref d) = room.persistence.debouncer {
        let _ = d.persist_tx.send(Some(persist_bytes));
    }
}

/// Check if the current metadata differs from kernel's launched config and broadcast sync state.
/// Called after metadata sync to notify all clients about dependency drift.
///
/// Handles two cases:
/// 1. Kernel launched with inline deps - track drift (additions/removals)
/// 2. Kernel launched with prewarmed - detect when user adds inline deps (needs restart)
pub(crate) async fn check_and_broadcast_sync_state(room: &NotebookRoom) {
    // Get current metadata from doc
    let current_metadata = {
        let doc = room.doc.read().await;
        doc.get_metadata_snapshot()
    };

    let Some(current_metadata) = current_metadata else {
        return;
    };

    // Read launched_config from room (set when runtime agent launched/restarted).
    // Clone the config and drop the guard before any `.await` to avoid holding
    // a lock across await points (deadlock prevention).
    let launched = {
        let launched_guard = room.runtime_agent_launched_config.read().await;
        match &*launched_guard {
            Some(l) => l.clone(),
            None => return,
        }
    };

    // Check kernel is actually running via RuntimeStateDoc
    {
        let lifecycle = room
            .state
            .read(|sd| sd.read_state().kernel.lifecycle)
            .unwrap_or(RuntimeLifecycle::NotStarted);
        if !matches!(lifecycle, RuntimeLifecycle::Running(_)) {
            return;
        }
    }

    // Check if we're tracking inline deps or deno config
    let is_tracking = launched.uv_deps.is_some()
        || launched.conda_deps.is_some()
        || launched.pixi_deps.is_some()
        || launched.pixi_toml_deps.is_some()
        || launched.deno_config.is_some();

    if is_tracking {
        // Kernel launched with inline deps - compute drift
        let diff = compute_env_sync_diff(&launched, &current_metadata);
        let _in_sync = diff.is_none();

        // Write to RuntimeStateDoc
        if let Err(e) = room.state.with_doc(|sd| match &diff {
            Some(d) => sd.set_env_sync(
                false,
                &d.added,
                &d.removed,
                d.channels_changed,
                d.deno_changed,
            ),
            None => sd.set_env_sync(true, &[], &[], false, false),
        }) {
            warn!("[runtime-state] {}", e);
        }
    } else {
        // Kernel launched with prewarmed - check if metadata now has inline deps
        let current_inline = check_inline_deps(&current_metadata);

        if let Some(ref inline_source) = current_inline {
            use notebook_protocol::connection::{EnvSource, PackageManager};
            let added = match inline_source {
                EnvSource::Inline(PackageManager::Uv) => {
                    get_inline_uv_deps(&current_metadata).unwrap_or_default()
                }
                EnvSource::Inline(PackageManager::Conda) => {
                    get_inline_conda_deps(&current_metadata).unwrap_or_default()
                }
                _ => vec![],
            };

            if !added.is_empty() {
                if let Err(e) = room
                    .state
                    .with_doc(|sd| sd.set_env_sync(false, &added, &[], false, false))
                {
                    warn!("[runtime-state] {}", e);
                }
            }
        }
    }
}

/// Re-verify trust from the Automerge doc and update room.trust_state + RuntimeStateDoc.
///
/// Called after every Automerge sync message to detect trust metadata changes.
/// Explicit approvals now flow through `NotebookRequest::ApproveTrust`, but
/// external file reloads and older docs can still update trust fields through
/// normal CRDT sync. Without this, room.trust_state would remain stale from
/// initial room creation and the trust banner would reappear on reconnection.
pub(crate) async fn check_and_update_trust_state(room: &NotebookRoom) {
    let current_metadata = {
        let doc = room.doc.read().await;
        doc.get_metadata_snapshot()
    };

    let Some(current_metadata) = current_metadata else {
        return;
    };

    let mut new_trust = verify_trust_from_snapshot(&current_metadata);
    enrich_trust_info(room, &mut new_trust);

    let current_status = {
        let ts = room.trust_state.read().await;
        ts.status.clone()
    };

    if matches!(new_trust.status, runt_trust::TrustStatus::Untrusted) {
        match room
            .trusted_packages
            .all_dependencies_approved(&new_trust.info)
        {
            Ok(true) => match auto_approve_allowlisted_snapshot(room).await {
                Ok(true) => return,
                Ok(false) => {}
                Err(e) => {
                    warn!(
                        "[trusted-packages] Failed to auto-approve allowlisted dependencies: {}",
                        e
                    );
                }
            },
            Ok(false) => {}
            Err(e) => {
                warn!(
                    "[trusted-packages] Failed to read package allowlist; trust remains untrusted: {}",
                    e
                );
            }
        }
    }

    // Auto re-sign: if the user has already approved this notebook, deps
    // changes shouldn't flip it back to SignatureInvalid (which would kill
    // the kernel). Re-compute the signature in place and keep the notebook
    // Trusted. This covers both the UI-driven dep add path (primary case)
    // and external file edits on an already-approved notebook.
    if matches!(current_status, runt_trust::TrustStatus::Trusted)
        && matches!(new_trust.status, runt_trust::TrustStatus::SignatureInvalid)
    {
        match resign_trusted_snapshot(room).await {
            Ok(true) => {
                // Fresh snapshot/state now reflect Trusted — nothing else to do.
                return;
            }
            Ok(false) => {
                // Nothing to re-sign (snapshot vanished). Fall through.
            }
            Err(e) => {
                warn!(
                    "[notebook-sync] Failed to auto re-sign trusted notebook, \
                     falling back to SignatureInvalid: {}",
                    e
                );
                // Fall through to the normal transition path.
            }
        }
    }

    if current_status != new_trust.status {
        info!(
            "[notebook-sync] Trust state changed via doc sync: {:?} -> {:?}",
            current_status, new_trust.status
        );

        // Update room.trust_state so auto-launch and reconnection use fresh state
        {
            let mut ts = room.trust_state.write().await;
            *ts = new_trust;
        }

        // Update RuntimeStateDoc so the frontend banner reacts immediately
        let ts = room.trust_state.read().await;
        write_trust_to_runtime_state(room, &ts);
    } else {
        let current_info = {
            let ts = room.trust_state.read().await;
            ts.info.clone()
        };
        if current_info.approved_uv_dependencies != new_trust.info.approved_uv_dependencies
            || current_info.approved_conda_dependencies
                != new_trust.info.approved_conda_dependencies
            || current_info.approved_pixi_dependencies != new_trust.info.approved_pixi_dependencies
            || current_info.approved_pixi_pypi_dependencies
                != new_trust.info.approved_pixi_pypi_dependencies
        {
            {
                let mut ts = room.trust_state.write().await;
                *ts = new_trust;
            }
            let ts = room.trust_state.read().await;
            write_trust_to_runtime_state(room, &ts);
        }
    }
}

async fn auto_approve_allowlisted_snapshot(room: &NotebookRoom) -> Result<bool, String> {
    let persist_bytes = {
        let mut doc = room.doc.write().await;
        let Some(mut snapshot) = doc.get_metadata_snapshot() else {
            return Ok(false);
        };

        let mut trust = verify_trust_from_snapshot(&snapshot);
        enrich_trust_info(room, &mut trust);
        if !matches!(trust.status, runt_trust::TrustStatus::Untrusted) {
            return Ok(false);
        }
        match room.trusted_packages.all_dependencies_approved(&trust.info) {
            Ok(true) => {}
            Ok(false) => return Ok(false),
            Err(e) => return Err(e.to_string()),
        }

        auto_sign_in_place(&mut snapshot)?;
        doc.set_metadata_snapshot(&snapshot)
            .map_err(|e| format!("Failed to write allowlist trust approval: {}", e))?;
        doc.save()
    };

    let _ = room.broadcasts.changed_tx.send(());
    if let Some(ref debouncer) = room.persistence.debouncer {
        let _ = debouncer.persist_tx.send(Some(persist_bytes));
    }

    let mut updated = {
        let doc = room.doc.read().await;
        let Some(snapshot) = doc.get_metadata_snapshot() else {
            return Ok(false);
        };
        verify_trust_from_snapshot(&snapshot)
    };
    enrich_trust_info(room, &mut updated);
    {
        let mut ts = room.trust_state.write().await;
        *ts = updated;
    }
    let ts = room.trust_state.read().await;
    write_trust_to_runtime_state(room, &ts);
    Ok(true)
}

/// Sign the snapshot's runt dependency metadata with the daemon's trust key
/// and write the signature + timestamp back into the snapshot in place.
///
/// Does not persist the snapshot — the caller writes it back to the doc.
/// Used by `resign_trusted_snapshot` (re-sign a previously-Trusted notebook
/// whose deps changed) and by explicit trust approval paths. Project-file deps
/// are runtime context and should not be signed unless they have been copied
/// into notebook metadata.
pub(crate) fn auto_sign_in_place(snapshot: &mut NotebookMetadataSnapshot) -> Result<(), String> {
    let runt_value = serde_json::to_value(&snapshot.runt)
        .map_err(|e| format!("serialize runt metadata: {}", e))?;
    let mut additional = std::collections::HashMap::new();
    additional.insert("runt".to_string(), runt_value);
    let signature = runt_trust::sign_notebook_dependencies(&additional)?;
    snapshot.runt.trust_signature = Some(signature);
    snapshot.runt.trust_timestamp = Some(chrono::Utc::now().to_rfc3339());
    Ok(())
}

/// Re-sign the notebook's current metadata with the daemon's trust key and
/// write the new signature back into the Automerge doc. Used when deps have
/// changed on a previously-Trusted notebook, so the user doesn't have to
/// re-approve through the UI for every dependency edit.
///
/// Returns `Ok(true)` if a re-sign was applied, `Ok(false)` if there was
/// nothing to sign (no metadata snapshot). Errors propagate so the caller
/// can fall back to the normal SignatureInvalid transition.
async fn resign_trusted_snapshot(room: &NotebookRoom) -> Result<bool, String> {
    let mut doc = room.doc.write().await;

    let Some(mut snapshot) = doc.get_metadata_snapshot() else {
        return Ok(false);
    };

    auto_sign_in_place(&mut snapshot)?;

    doc.fork_and_merge(|fork| {
        let _ = fork.set_metadata_snapshot(&snapshot);
    });
    drop(doc);

    // Persist the new signature to disk so reopening after a restart keeps
    // the notebook Trusted without another sync round-trip.
    let _ = room.broadcasts.changed_tx.send(());

    info!("[notebook-sync] Auto re-signed trusted notebook after deps change");
    Ok(true)
}

/// Resolve the metadata snapshot for a notebook, trying the Automerge doc first
/// and falling back to disk if the doc doesn't have metadata yet (e.g., before
/// the first client has synced).
pub(crate) async fn resolve_metadata_snapshot(
    room: &NotebookRoom,
    notebook_path: Option<&Path>,
) -> Option<NotebookMetadataSnapshot> {
    // Try reading from the Automerge doc first
    {
        let doc = room.doc.read().await;
        if let Some(snapshot) = doc.get_metadata_snapshot() {
            debug!("[notebook-sync] Resolved metadata snapshot from Automerge doc");
            return Some(snapshot);
        }
    }

    // Fall back to reading from disk
    if let Some(path) = notebook_path {
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Ok(nb) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(metadata) = nb.get("metadata") {
                    let snapshot = NotebookMetadataSnapshot::from_metadata_value(metadata);
                    debug!("[notebook-sync] Resolved metadata snapshot from disk");
                    return Some(snapshot);
                }
            }
        }
    }

    None
}

/// If the detected project file is an `environment.yml` whose target
/// conda env isn't built on this machine, return the build decision details.
///
/// The auto-launch path uses this to detect the miss upfront and set a
/// specific error lifecycle (with `KernelErrorReason::CondaEnvYmlMissing`
/// and human-readable details) instead of letting the runtime agent
/// die with a generic "could not resolve conda environment prefix" and
/// leaving the kernel stuck in `initializing` forever. Covers #2157.
///
/// Returns `None` when the project file isn't `environment.yml` or when
/// the env is already built and usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CondaEnvYmlBuildDecision {
    pub label: String,
    pub create_command: String,
}

pub(crate) fn missing_conda_env_yml_decision(
    detected: &crate::project_file::DetectedProjectFile,
) -> Option<CondaEnvYmlBuildDecision> {
    if detected.kind != crate::project_file::ProjectFileKind::EnvironmentYml {
        return None;
    }

    let config = crate::project_file::parse_environment_yml(&detected.path).ok()?;
    let conda_prefix = if let Some(ref prefix) = config.prefix {
        prefix.clone()
    } else if let Some(ref name) = config.name {
        crate::project_file::find_named_conda_env(name)
            .unwrap_or_else(|| crate::project_file::default_conda_envs_dir().join(name))
    } else {
        let cache_dir = crate::paths::default_cache_dir().join("conda-envs");
        let conda_deps_tmp = kernel_env::CondaDependencies {
            dependencies: config.dependencies.clone(),
            channels: config.channels.clone(),
            python: config.python.clone(),
            env_id: None,
        };
        cache_dir.join(kernel_env::conda::compute_env_hash(&conda_deps_tmp))
    };

    if crate::project_file::conda_python_path(&conda_prefix).exists() {
        return None;
    }

    let label = config
        .name
        .clone()
        .unwrap_or_else(|| conda_prefix.display().to_string());
    let create_command = if config.name.is_some() && config.prefix.is_none() {
        format!("conda env create -f {}", detected.path.display())
    } else {
        format!(
            "conda env create -p {} -f {}",
            conda_prefix.display(),
            detected.path.display()
        )
    };

    Some(CondaEnvYmlBuildDecision {
        label,
        create_command,
    })
}

pub(crate) fn format_conda_env_yml_build_details(decision: &CondaEnvYmlBuildDecision) -> String {
    format!(
        "environment.yml declares conda env '{}', which is not built on this machine. Run: {}",
        decision.label, decision.create_command
    )
}

pub(crate) fn project_environment_build_approved(
    room: &NotebookRoom,
    detected: &crate::project_file::DetectedProjectFile,
) -> bool {
    let Ok(config) = crate::project_file::parse_environment_yml(&detected.path) else {
        return false;
    };
    let trust_info = environment_yml_trust_info(&config);
    match room.trusted_packages.all_dependencies_approved(&trust_info) {
        Ok(approved) => approved,
        Err(error) => {
            warn!(
                "[trusted-packages] Failed to check project environment approval for {:?}: {}",
                detected.path, error
            );
            false
        }
    }
}

pub(crate) fn environment_yml_trust_info(
    config: &crate::project_file::EnvironmentYmlConfig,
) -> runt_trust::TrustInfo {
    runt_trust::TrustInfo {
        status: runt_trust::TrustStatus::Untrusted,
        uv_dependencies: vec![],
        approved_uv_dependencies: vec![],
        conda_dependencies: config.dependencies.clone(),
        approved_conda_dependencies: vec![],
        conda_channels: config.channels.clone(),
        pixi_dependencies: vec![],
        approved_pixi_dependencies: vec![],
        pixi_pypi_dependencies: vec![],
        approved_pixi_pypi_dependencies: vec![],
        pixi_channels: vec![],
    }
}

/// Check whether a notebook's inline dependency list exactly matches a
/// project file's dependency list (pyproject.toml / environment.yml /
/// pixi.toml) in the notebook's own directory.
///
/// When this is true, the notebook's deps are an exact mirror of a file
/// the user already has on disk. Signing them is safe because the trust
/// surface ("these deps came from this machine") is already satisfied by
/// file ownership. Used by #2150's room-init and streaming-load
/// reconciliation to heal notebooks saved by pre-fix builds whose
/// daemon-written deps never got a signature.
///
/// Returns `false` when no project file is found, when no kind matches
/// the notebook's populated dep section, or when the lists differ.
///
/// Side-effect-free: does not read the notebook itself, does not touch
/// the trust key, does not write anywhere. Callers decide what to do
/// with the match.
pub(crate) fn project_file_deps_match_trust_info(
    notebook_path: &Path,
    info: &runt_trust::TrustInfo,
) -> bool {
    let Some(parent) = notebook_path.parent() else {
        return false;
    };
    let Some(detected) = crate::project_file::detect_project_file(parent) else {
        return false;
    };
    // Order-insensitive compare: `extract_pyproject_deps` already sorts,
    // but a hand-edited .ipynb may hold deps in any order. Sort both
    // sides so the match is stable regardless of source ordering.
    fn sorted(xs: &[String]) -> Vec<String> {
        let mut v = xs.to_vec();
        v.sort();
        v
    }
    match detected.kind {
        crate::project_file::ProjectFileKind::PyprojectToml => {
            let Ok(content) = std::fs::read_to_string(&detected.path) else {
                return false;
            };
            let project_deps = extract_pyproject_deps(&content);
            !project_deps.is_empty() && sorted(&project_deps) == sorted(&info.uv_dependencies)
        }
        crate::project_file::ProjectFileKind::EnvironmentYml => {
            let Ok(env_config) = crate::project_file::parse_environment_yml(&detected.path) else {
                return false;
            };
            // Channels are part of the signed trust payload (runt_trust
            // extracts them into TrustInfo) and conda channel priority
            // affects package resolution, so channel order matters.
            // Compare exact, preserving order. A notebook with matching
            // deps but different channels stays Untrusted — that's a
            // real trust event, not a silent heal.
            !env_config.dependencies.is_empty()
                && sorted(&env_config.dependencies) == sorted(&info.conda_dependencies)
                && env_config.channels == info.conda_channels
        }
        crate::project_file::ProjectFileKind::PixiToml => {
            // Keep pixi.toml reconciliation conservative: inline
            // `metadata.runt.pixi` is trust-checked, but auto-signing an
            // unsigned notebook from a project file needs an exact manifest
            // comparison that preserves version specs and PyPI/conda splits.
            false
        }
    }
}

/// Verify trust status of a notebook by reading its file from disk.
/// Returns TrustState with the verification result.
///
/// Used during room creation when the Automerge doc is still empty.
/// Once the doc is populated, `verify_trust_from_snapshot` is preferred
/// as it picks up in-memory changes (e.g., newly-written trust signatures).
pub(crate) fn verify_trust_from_file(notebook_path: &Path) -> TrustState {
    // Read and parse the notebook file
    let metadata = match std::fs::read_to_string(notebook_path) {
        Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
            Ok(nb) => nb
                .get("metadata")
                .and_then(|m| m.as_object())
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default(),
            Err(_) => std::collections::HashMap::new(),
        },
        Err(_) => std::collections::HashMap::new(),
    };

    // Verify trust using the shared runt-trust crate
    match runt_trust::verify_notebook_trust(&metadata) {
        Ok(info) => TrustState {
            status: info.status.clone(),
            info,
            pending_launch: false,
        },
        Err(_) => TrustState {
            status: runt_trust::TrustStatus::Untrusted,
            info: runt_trust::TrustInfo {
                status: runt_trust::TrustStatus::Untrusted,
                uv_dependencies: vec![],
                approved_uv_dependencies: vec![],
                conda_dependencies: vec![],
                approved_conda_dependencies: vec![],
                conda_channels: vec![],
                pixi_dependencies: vec![],
                approved_pixi_dependencies: vec![],
                pixi_pypi_dependencies: vec![],
                approved_pixi_pypi_dependencies: vec![],
                pixi_channels: vec![],
            },
            pending_launch: false,
        },
    }
}

/// Verify trust status from a `NotebookMetadataSnapshot` (from the Automerge doc).
///
/// This provides the same trust verification as `verify_trust_from_file` but
/// works with the in-memory doc state instead of reading from disk. Used by
/// `check_and_update_trust_state` to detect trust changes reactively (e.g.,
/// after the frontend writes a trust signature via approval).
pub(crate) fn verify_trust_from_snapshot(snapshot: &NotebookMetadataSnapshot) -> TrustState {
    // Build a metadata HashMap from the snapshot's runt field, matching the
    // structure that runt_trust::verify_notebook_trust expects.
    //
    // We only insert the "runt" key — legacy top-level "uv"/"conda" keys are
    // already normalized into runt.uv/runt.conda by
    // NotebookMetadataSnapshot::from_metadata_value before they reach the
    // Automerge doc, so the legacy fallback in get_uv_metadata is not needed.
    let mut metadata = std::collections::HashMap::new();
    if let Ok(runt_value) = serde_json::to_value(&snapshot.runt) {
        metadata.insert("runt".to_string(), runt_value);
    }

    match runt_trust::verify_notebook_trust(&metadata) {
        Ok(info) => TrustState {
            status: info.status.clone(),
            info,
            pending_launch: false,
        },
        Err(_) => TrustState {
            status: runt_trust::TrustStatus::Untrusted,
            info: runt_trust::TrustInfo {
                status: runt_trust::TrustStatus::Untrusted,
                uv_dependencies: vec![],
                approved_uv_dependencies: vec![],
                conda_dependencies: vec![],
                approved_conda_dependencies: vec![],
                conda_channels: vec![],
                pixi_dependencies: vec![],
                approved_pixi_dependencies: vec![],
                pixi_pypi_dependencies: vec![],
                approved_pixi_pypi_dependencies: vec![],
                pixi_channels: vec![],
            },
            pending_launch: false,
        },
    }
}

/// Which runtime this capture applies to — UV pool envs or Conda pool envs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapturedEnvRuntime {
    Uv,
    Conda,
}

/// Write captured user-level deps and env_id into the notebook's metadata
/// if not already set.
///
/// This is the first-launch capture step from the unified env resolution
/// design: after the daemon takes an env from the pool and claims it at the
/// unified-hash path, the notebook's metadata records the user-level deps
/// derived from the pool env's install list (base packages stripped). On
/// subsequent reopens, this captured set participates in the hash lookup
/// so the claimed env is found on disk.
///
/// Write-once semantics:
/// - `env_id` is set only if the metadata's `env_id` is `None`.
/// - `dependencies` on `metadata.runt.uv` or `metadata.runt.conda` is
///   overwritten only if empty (the brand-new notebook case). Existing
///   non-empty deps are left alone so user-edited lists aren't clobbered.
///
/// Uses `fork_and_merge` per the async-CRDT-mutation rule.
///
/// Returns `true` if any field was updated.
pub(crate) async fn capture_env_into_metadata(
    room: &NotebookRoom,
    runtime: CapturedEnvRuntime,
    user_defaults: &[String],
    env_id: &str,
) -> bool {
    let mut changed = false;
    let mut doc = room.doc.write().await;
    doc.fork_and_merge(|fork| {
        let mut snap = fork.get_metadata_snapshot().unwrap_or_default();

        if snap.runt.env_id.is_none() {
            snap.runt.env_id = Some(env_id.to_string());
            changed = true;
        }

        match runtime {
            CapturedEnvRuntime::Uv => {
                let uv =
                    snap.runt
                        .uv
                        .get_or_insert_with(|| notebook_doc::metadata::UvInlineMetadata {
                            dependencies: Vec::new(),
                            requires_python: None,
                            prerelease: None,
                        });
                if uv.dependencies.is_empty() && !user_defaults.is_empty() {
                    uv.dependencies = user_defaults.to_vec();
                    changed = true;
                }
            }
            CapturedEnvRuntime::Conda => {
                let conda = snap.runt.conda.get_or_insert_with(|| {
                    notebook_doc::metadata::CondaInlineMetadata {
                        dependencies: Vec::new(),
                        channels: vec!["conda-forge".to_string()],
                        python: None,
                    }
                });
                if conda.dependencies.is_empty() && !user_defaults.is_empty() {
                    conda.dependencies = user_defaults.to_vec();
                    changed = true;
                }
            }
        }

        if changed {
            let _ = fork.set_metadata_snapshot(&snap);
        }
    });
    drop(doc);
    if changed {
        // Notify the autosave debouncer so the capture lands in the .ipynb
        // even when the user closes the notebook without any other edits.
        // Without this, captured metadata lives only in the in-memory CRDT
        // and evaporates on room eviction, making the next reopen re-capture
        // from scratch.
        let _ = room.broadcasts.changed_tx.send(());
    }
    changed
}

/// Derive the user-level dep list that should land in metadata for the
/// given runtime, based on the launched config currently running.
///
/// `launched.uv_deps` / `launched.conda_deps` are the source of truth
/// while the kernel is up. They're populated at launch time for
/// captured reopens and inline-deps launches, and the hot-sync handler
/// always appends synced packages to the matching field before
/// returning success. So if `launched.uv_deps` is `None`, no UV hot
/// sync occurred and captured metadata already matches the on-disk env
/// — no flush needed for UV. Same for Conda.
///
/// `strip_base` removes the runtime's base packages so the persisted
/// list reflects only user intent.
///
/// Returns `None` for runtimes that didn't participate in the launch
/// (the other runtime's field, deno-only launches, pixi launches).
pub(crate) fn effective_user_deps_from_launched(
    launched: &LaunchedEnvConfig,
    runtime: CapturedEnvRuntime,
) -> Option<Vec<String>> {
    match runtime {
        CapturedEnvRuntime::Uv => {
            let deps = launched.uv_deps.as_ref()?;
            Some(kernel_env::strip_base(
                deps,
                kernel_env::uv::UV_BASE_PACKAGES,
            ))
        }
        CapturedEnvRuntime::Conda => {
            let deps = launched.conda_deps.as_ref()?;
            Some(kernel_env::strip_base(
                deps,
                kernel_env::conda::CONDA_BASE_PACKAGES,
            ))
        }
    }
}

/// Write the post-hot-sync dep list back to `metadata.runt.{uv,conda}.
/// dependencies` if it drifted from what the kernel actually has
/// installed. Called at eviction time so the saved `.ipynb` reflects
/// every package the user hot-installed during the session.
///
/// Returns `true` if metadata was updated (caller should persist the
/// notebook to disk before the room is torn down).
pub(crate) async fn flush_launched_deps_to_metadata(
    room: &NotebookRoom,
    launched: &LaunchedEnvConfig,
    runtime: CapturedEnvRuntime,
) -> bool {
    let Some(new_deps) = effective_user_deps_from_launched(launched, runtime) else {
        return false;
    };

    let mut changed = false;
    let mut doc = room.doc.write().await;
    doc.fork_and_merge(|fork| {
        let mut snap = fork.get_metadata_snapshot().unwrap_or_default();
        match runtime {
            CapturedEnvRuntime::Uv => {
                let uv =
                    snap.runt
                        .uv
                        .get_or_insert_with(|| notebook_doc::metadata::UvInlineMetadata {
                            dependencies: Vec::new(),
                            requires_python: None,
                            prerelease: None,
                        });
                if uv.dependencies != new_deps {
                    uv.dependencies = new_deps.clone();
                    changed = true;
                }
            }
            CapturedEnvRuntime::Conda => {
                let conda = snap.runt.conda.get_or_insert_with(|| {
                    notebook_doc::metadata::CondaInlineMetadata {
                        dependencies: Vec::new(),
                        channels: vec!["conda-forge".to_string()],
                        python: None,
                    }
                });
                if conda.dependencies != new_deps {
                    conda.dependencies = new_deps.clone();
                    changed = true;
                }
            }
        }
        if changed {
            let _ = fork.set_metadata_snapshot(&snap);
        }
    });
    drop(doc);
    changed
}

/// Full dep-shape captured in notebook metadata plus its env_id.
///
/// Carries every resolver-affecting field so the unified-hash lookup on
/// reopen matches the hash the capture step wrote. Dropping any of these
/// fields (e.g. reducing to just `dependencies`) would make the hash diverge
/// whenever a user edits `runt.uv.prerelease`, `runt.uv.requires-python`,
/// `runt.conda.channels`, or `runt.conda.python` after capture.
#[derive(Debug, Clone)]
pub(crate) enum CapturedEnv {
    Uv {
        deps: kernel_env::UvDependencies,
        env_id: String,
    },
    Conda {
        deps: kernel_env::CondaDependencies,
        env_id: String,
    },
}

impl CapturedEnv {
    #[allow(dead_code)]
    pub(crate) fn env_id(&self) -> &str {
        match self {
            CapturedEnv::Uv { env_id, .. } | CapturedEnv::Conda { env_id, .. } => env_id,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn dependencies(&self) -> &[String] {
        match self {
            CapturedEnv::Uv { deps, .. } => &deps.dependencies,
            CapturedEnv::Conda { deps, .. } => &deps.dependencies,
        }
    }
}

/// Pull the captured env shape (full dep spec + env_id) out of a metadata
/// snapshot. Returns `None` if no env_id is set.
///
/// Extracts every field that feeds into `compute_unified_env_hash`:
/// - UV: `dependencies`, `requires-python`, `prerelease`
/// - Conda: `dependencies`, `channels`, `python`
///
/// Missing `runt.uv` / `runt.conda` sections yield default (empty) deps with
/// `None` resolver fields, which matches the on-disk hash at first-launch
/// capture time (when those sections are written with defaults).
pub(crate) fn captured_env_for_runtime(
    snapshot: Option<&NotebookMetadataSnapshot>,
    runtime: CapturedEnvRuntime,
) -> Option<CapturedEnv> {
    let snap = snapshot?;
    let env_id = snap.runt.env_id.as_ref()?.clone();
    match runtime {
        CapturedEnvRuntime::Uv => {
            let (dependencies, requires_python, prerelease) = snap
                .runt
                .uv
                .as_ref()
                .map(|u| {
                    (
                        u.dependencies.clone(),
                        u.requires_python.clone(),
                        u.prerelease.clone(),
                    )
                })
                .unwrap_or_else(|| (Vec::new(), None, None));
            Some(CapturedEnv::Uv {
                deps: kernel_env::UvDependencies {
                    dependencies,
                    requires_python,
                    prerelease,
                },
                env_id,
            })
        }
        CapturedEnvRuntime::Conda => {
            let (dependencies, channels, python) = snap
                .runt
                .conda
                .as_ref()
                .map(|c| {
                    let channels = if c.channels.is_empty() {
                        vec!["conda-forge".to_string()]
                    } else {
                        c.channels.clone()
                    };
                    (c.dependencies.clone(), channels, c.python.clone())
                })
                .unwrap_or_else(|| (Vec::new(), vec!["conda-forge".to_string()], None));
            Some(CapturedEnv::Conda {
                deps: kernel_env::CondaDependencies {
                    dependencies,
                    channels,
                    python,
                    env_id: None,
                },
                env_id,
            })
        }
    }
}

/// Check whether a captured env exists on disk at the unified-hash path.
/// Returns the cache path + python path if present.
pub(crate) fn unified_env_on_disk(captured: &CapturedEnv) -> Option<(PathBuf, PathBuf)> {
    unified_env_on_disk_in(
        captured,
        &kernel_env::uv::default_cache_dir_uv(),
        &kernel_env::conda::default_cache_dir_conda(),
    )
}

/// Test-friendly form of [`unified_env_on_disk`] that accepts explicit
/// cache dirs instead of using the channel defaults. The production call
/// site always passes the defaults; tests pass tmpdirs.
fn unified_env_on_disk_in(
    captured: &CapturedEnv,
    uv_cache_dir: &Path,
    conda_cache_dir: &Path,
) -> Option<(PathBuf, PathBuf)> {
    match captured {
        CapturedEnv::Uv { deps, env_id } => {
            let hash = kernel_env::uv::compute_unified_env_hash(deps, env_id);
            let venv_path = uv_cache_dir.join(&hash);

            #[cfg(target_os = "windows")]
            let python_path = venv_path.join("Scripts").join("python.exe");
            #[cfg(not(target_os = "windows"))]
            let python_path = venv_path.join("bin").join("python");

            if python_path.exists() {
                Some((venv_path, python_path))
            } else {
                None
            }
        }
        CapturedEnv::Conda { deps, env_id } => {
            let hash = kernel_env::conda::compute_unified_env_hash(deps, env_id);
            let env_path = conda_cache_dir.join(&hash);

            #[cfg(target_os = "windows")]
            let python_path = env_path.join("python.exe");
            #[cfg(not(target_os = "windows"))]
            let python_path = env_path.join("bin").join("python");

            if python_path.exists() {
                Some((env_path, python_path))
            } else {
                None
            }
        }
    }
}

/// Decide whether to preserve the room's env directory on eviction.
///
/// Rule: if the notebook has a saved path on disk AND its current
/// `env_path` matches the captured unified-hash env path in metadata,
/// keep the env. Saved path means the `.ipynb` will persist the env_id
/// binding, so the on-disk cache is reachable on next open.
///
/// Untitled / never-saved rooms have no persistent reference and their
/// env is orphaned on eviction — delete as before.
///
/// Non-captured envs (pool / legacy inline) are always cleaned because
/// their paths don't survive re-launch regardless.
pub(crate) fn should_preserve_env_on_eviction(
    has_saved_path: bool,
    env_path: &Path,
    metadata: Option<&NotebookMetadataSnapshot>,
    uv_cache_dir: &Path,
    conda_cache_dir: &Path,
) -> bool {
    if !has_saved_path {
        return false;
    }
    for runtime in [CapturedEnvRuntime::Uv, CapturedEnvRuntime::Conda] {
        if let Some(captured) = captured_env_for_runtime(metadata, runtime) {
            if let Some((venv, _)) =
                unified_env_on_disk_in(&captured, uv_cache_dir, conda_cache_dir)
            {
                if venv == env_path {
                    return true;
                }
            }
        }
    }
    false
}

/// Compute the target path the env dir should live at, given the
/// runtime the kernel was actually launched with. Returns `None` when
/// the requested runtime has no captured env in metadata.
///
/// The explicit `runtime` argument avoids the UV-first metadata-probe
/// bug: a pure-conda notebook has `runt.env_id` set and
/// `captured_env_for_runtime(Uv)` happily synthesises an empty UV
/// capture, so without the runtime hint the old version would route
/// conda eviction through a UV hash.
fn unified_hash_target_path(
    metadata: Option<&NotebookMetadataSnapshot>,
    runtime: CapturedEnvRuntime,
    uv_cache_dir: &Path,
    conda_cache_dir: &Path,
) -> Option<PathBuf> {
    let captured = captured_env_for_runtime(metadata, runtime)?;
    let (hash, dir) = match &captured {
        CapturedEnv::Uv { deps, env_id } => (
            kernel_env::uv::compute_unified_env_hash(deps, env_id),
            uv_cache_dir,
        ),
        CapturedEnv::Conda { deps, env_id } => (
            kernel_env::conda::compute_unified_env_hash(deps, env_id),
            conda_cache_dir,
        ),
    };
    Some(dir.join(&hash))
}

/// Rename the env dir on disk to match the captured metadata's unified
/// hash for the given runtime. Called at eviction after the hot-sync
/// flush updates metadata deps: the on-disk dir is still named for the
/// pre-flush hash, so we rename it before the next reopen's cache
/// lookup.
///
/// Returns the path after the rename. Callers should use this as the
/// authoritative env path for any later steps (e.g. the preserve check).
///
/// Skips the rename when:
/// - The current path already matches the target hash (no drift).
/// - The source path no longer exists (already cleaned up).
/// - The target path already exists (would clobber another env — log
///   and keep the source).
pub(crate) async fn rename_env_dir_to_unified_hash(
    current_env_path: &Path,
    metadata: Option<&NotebookMetadataSnapshot>,
    runtime: CapturedEnvRuntime,
    uv_cache_dir: &Path,
    conda_cache_dir: &Path,
) -> PathBuf {
    let Some(target) = unified_hash_target_path(metadata, runtime, uv_cache_dir, conda_cache_dir)
    else {
        return current_env_path.to_path_buf();
    };
    if target == current_env_path {
        return current_env_path.to_path_buf();
    }
    if !current_env_path.exists() {
        return current_env_path.to_path_buf();
    }
    if target.exists() {
        warn!(
            "[notebook-sync] Unified-hash rename target {:?} already exists; leaving env at {:?}",
            target, current_env_path
        );
        return current_env_path.to_path_buf();
    }
    match tokio::fs::rename(current_env_path, &target).await {
        Ok(()) => {
            info!(
                "[notebook-sync] Renamed env {:?} -> {:?} to match new unified hash",
                current_env_path, target
            );
            target
        }
        Err(e) => {
            warn!(
                "[notebook-sync] Failed to rename env {:?} -> {:?}: {}",
                current_env_path, target, e
            );
            current_env_path.to_path_buf()
        }
    }
}

/// Return a captured-env `env_source` override (`uv:prewarmed` /
/// `conda:prewarmed`) when the notebook has captured deps + env_id and a
/// matching unified-hash env exists on disk. None otherwise.
///
/// This is the hinge that keeps captured notebooks on the prewarmed
/// capture path on reopen, even when `check_inline_deps` would otherwise
/// route them through the inline-deps flow based on their captured
/// (non-empty) dep list.
pub(crate) fn captured_env_source_override(
    metadata_snapshot: Option<&NotebookMetadataSnapshot>,
) -> Option<notebook_protocol::connection::EnvSource> {
    resolve_captured_env_override(metadata_snapshot).0
}

/// A notebook is considered "captured" only when its claimed env is still
/// present on disk. Deps-present-but-disk-absent is intentionally NOT
/// captured — we can't distinguish a GC'd captured env from a fresh
/// notebook whose user added inline deps before the first launch. Treating
/// the latter as captured would bypass the cross-notebook inline-deps
/// cache. The tradeoff: GC'd captured envs rebuild via the normal inline
/// path using legacy hashing. Same deps still dedup across notebooks; they
/// just lose per-notebook env_id isolation for that rebuild.
fn is_captured(captured: &CapturedEnv) -> bool {
    unified_env_on_disk(captured).is_some()
}

/// Like `captured_env_source_override` but also returns the full
/// `CapturedEnv` that matched. The capture data feeds into
/// `build_launched_config` so drift detection knows what the launch
/// baseline is.
fn resolve_captured_env_override(
    metadata_snapshot: Option<&NotebookMetadataSnapshot>,
) -> (
    Option<notebook_protocol::connection::EnvSource>,
    Option<CapturedEnv>,
) {
    use notebook_protocol::connection::{EnvSource, PackageManager};
    let Some(snap) = metadata_snapshot else {
        return (None, None);
    };
    if let Some(captured) = captured_env_for_runtime(Some(snap), CapturedEnvRuntime::Uv) {
        if is_captured(&captured) {
            return (
                Some(EnvSource::Prewarmed(PackageManager::Uv)),
                Some(captured),
            );
        }
    }
    if let Some(captured) = captured_env_for_runtime(Some(snap), CapturedEnvRuntime::Conda) {
        if is_captured(&captured) {
            return (
                Some(EnvSource::Prewarmed(PackageManager::Conda)),
                Some(captured),
            );
        }
    }
    (None, None)
}

/// Acquire a prewarmed env for a notebook, handling both the reopen
/// (captured-deps) path and the first-launch (pool-take + capture) path.
///
/// Returns `Some(Some(env))` with a PooledEnv wrapping either the claimed
/// reopen env or the newly-claimed pool env. Returns `Some(None)` when the
/// pool is empty but the caller should continue (unused by the prewarmed
/// paths today). Returns `None` when a fatal error was broadcast and the
/// caller should unwind.
pub(crate) async fn acquire_prewarmed_env_with_capture(
    env_source: &str,
    daemon: &std::sync::Arc<crate::daemon::Daemon>,
    room: &NotebookRoom,
    metadata_snapshot: Option<&NotebookMetadataSnapshot>,
) -> Option<Option<crate::PooledEnv>> {
    let runtime = match env_source {
        "uv:prewarmed" => CapturedEnvRuntime::Uv,
        "conda:prewarmed" => CapturedEnvRuntime::Conda,
        _ => return acquire_pool_env_for_source(env_source, daemon, room).await,
    };
    let progress_handler: std::sync::Arc<dyn kernel_env::ProgressHandler> =
        std::sync::Arc::new(crate::inline_env::BroadcastProgressHandler::with_state(
            room.broadcasts.kernel_broadcast_tx.clone(),
            room.state.clone(),
        ));

    // Reopen path: if the notebook has an env_id and the unified-hash env
    // exists on disk, route through prepare_environment_unified for an
    // instant cache hit. Captured deps + env_id → same env across reopens.
    //
    // Uses the FULL dep-shape from metadata (requires-python, prerelease,
    // channels, python pin) so the hash matches what the capture step wrote,
    // even after the user edits one of those resolver-affecting fields.
    //
    // `is_captured` checks for the env on disk — deps-present-but-disk-absent
    // falls through to the pool-take + capture path. That covers both fresh
    // notebooks (empty deps) and GC'd captured envs (non-empty deps with no
    // on-disk presence). The GC'd case accepts a rebuild via the normal
    // inline path rather than routing through unified hashing, to avoid
    // conflating "captured" with "user added inline deps before first launch."
    if let Some(captured) = captured_env_for_runtime(metadata_snapshot, runtime) {
        if is_captured(&captured) {
            match &captured {
                CapturedEnv::Uv { deps, env_id } => {
                    let cache_dir = kernel_env::uv::default_cache_dir_uv();
                    match kernel_env::uv::prepare_environment_unified(
                        deps,
                        env_id,
                        &cache_dir,
                        progress_handler.clone(),
                    )
                    .await
                    {
                        Ok(prepared) => {
                            info!(
                                "[notebook-sync] Reopen cache-hit for env_id={} at {:?}",
                                env_id, prepared.venv_path
                            );
                            return Some(Some(crate::PooledEnv {
                                env_type: crate::EnvType::Uv,
                                venv_path: prepared.venv_path,
                                python_path: prepared.python_path,
                                prewarmed_packages: deps.dependencies.clone(),
                            }));
                        }
                        Err(e) => {
                            warn!(
                                "[notebook-sync] Unified UV reopen failed ({}), falling back to pool",
                                e
                            );
                        }
                    }
                }
                CapturedEnv::Conda { deps, env_id } => {
                    let cache_dir = kernel_env::conda::default_cache_dir_conda();
                    match kernel_env::conda::prepare_environment_unified(
                        deps,
                        env_id,
                        &cache_dir,
                        progress_handler.clone(),
                    )
                    .await
                    {
                        Ok(prepared) => {
                            info!(
                                "[notebook-sync] Reopen cache-hit for env_id={} at {:?}",
                                env_id, prepared.env_path
                            );
                            return Some(Some(crate::PooledEnv {
                                env_type: crate::EnvType::Conda,
                                venv_path: prepared.env_path,
                                python_path: prepared.python_path,
                                prewarmed_packages: deps.dependencies.clone(),
                            }));
                        }
                        Err(e) => {
                            warn!(
                                "[notebook-sync] Unified conda reopen failed ({}), falling back to pool",
                                e
                            );
                        }
                    }
                }
            }
        }
    }

    // First-launch path: take from pool, strip base to derive user_defaults,
    // claim to the unified-hash location, and capture into metadata.
    let pooled = acquire_pool_env_for_source(env_source, daemon, room).await?;
    let pooled = pooled?;
    let leased_path = pooled.venv_path.clone();

    let env_id = metadata_snapshot
        .and_then(|s| s.runt.env_id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    match runtime {
        CapturedEnvRuntime::Uv => {
            let user_defaults =
                kernel_env::strip_base(&pooled.prewarmed_packages, kernel_env::UV_BASE_PACKAGES);
            let prewarmed = kernel_env::uv::UvEnvironment {
                venv_path: pooled.venv_path.clone(),
                python_path: pooled.python_path.clone(),
            };
            let cache_dir = kernel_env::uv::default_cache_dir_uv();
            match kernel_env::uv::claim_prewarmed_environment_in(
                prewarmed,
                &env_id,
                &user_defaults,
                &cache_dir,
            )
            .await
            {
                Ok(claimed) => {
                    let claimed_path = claimed.venv_path.clone();
                    let python_path = claimed.python_path.clone();
                    let _wrote = capture_env_into_metadata(
                        room,
                        CapturedEnvRuntime::Uv,
                        &user_defaults,
                        &env_id,
                    )
                    .await;
                    info!(
                        "[notebook-sync] Captured prewarmed UV env into metadata for env_id={} at {:?}",
                        env_id, claimed_path
                    );
                    daemon
                        .release_pool_lease(crate::EnvType::Uv, &leased_path)
                        .await;
                    Some(Some(crate::PooledEnv {
                        env_type: crate::EnvType::Uv,
                        venv_path: claimed_path,
                        python_path,
                        prewarmed_packages: pooled.prewarmed_packages,
                    }))
                }
                Err(e) => {
                    warn!(
                        "[notebook-sync] Failed to claim UV pool env ({}), using raw pool env",
                        e
                    );
                    {
                        let mut ep = room.runtime_agent_env_path.write().await;
                        *ep = Some(pooled.venv_path.clone());
                    }
                    daemon
                        .release_pool_lease(crate::EnvType::Uv, &leased_path)
                        .await;
                    Some(Some(pooled))
                }
            }
        }
        CapturedEnvRuntime::Conda => {
            let user_defaults =
                kernel_env::strip_base(&pooled.prewarmed_packages, kernel_env::CONDA_BASE_PACKAGES);
            let prewarmed = kernel_env::conda::CondaEnvironment {
                env_path: pooled.venv_path.clone(),
                python_path: pooled.python_path.clone(),
            };
            let cache_dir = kernel_env::conda::default_cache_dir_conda();
            match kernel_env::conda::claim_prewarmed_environment_in(
                prewarmed,
                &env_id,
                &user_defaults,
                &cache_dir,
            )
            .await
            {
                Ok(claimed) => {
                    let claimed_path = claimed.env_path.clone();
                    let python_path = claimed.python_path.clone();
                    let _wrote = capture_env_into_metadata(
                        room,
                        CapturedEnvRuntime::Conda,
                        &user_defaults,
                        &env_id,
                    )
                    .await;
                    info!(
                        "[notebook-sync] Captured prewarmed conda env into metadata for env_id={} at {:?}",
                        env_id, claimed_path
                    );
                    daemon
                        .release_pool_lease(crate::EnvType::Conda, &leased_path)
                        .await;
                    Some(Some(crate::PooledEnv {
                        env_type: crate::EnvType::Conda,
                        venv_path: claimed_path,
                        python_path,
                        prewarmed_packages: pooled.prewarmed_packages,
                    }))
                }
                Err(e) => {
                    warn!(
                        "[notebook-sync] Failed to claim conda pool env ({}), using raw pool env",
                        e
                    );
                    {
                        let mut ep = room.runtime_agent_env_path.write().await;
                        *ep = Some(pooled.venv_path.clone());
                    }
                    daemon
                        .release_pool_lease(crate::EnvType::Conda, &leased_path)
                        .await;
                    Some(Some(pooled))
                }
            }
        }
    }
}

/// Acquire a pooled environment from the appropriate pool based on env_source.
/// Returns None and broadcasts error if pool is empty.
async fn acquire_pool_env_for_source(
    env_source: &str,
    daemon: &std::sync::Arc<crate::daemon::Daemon>,
    _room: &NotebookRoom,
) -> Option<Option<crate::PooledEnv>> {
    use notebook_protocol::connection::{EnvSource, PackageManager};
    let parsed = EnvSource::parse(env_source);
    // Route to appropriate pool based on the resolved manager family.
    if matches!(parsed, EnvSource::Prewarmed(PackageManager::Pixi)) {
        match daemon.take_pixi_env().await {
            Some(env) => {
                info!(
                    "[notebook-sync] Acquired Pixi env from pool: {:?}",
                    env.python_path
                );
                return Some(Some(env));
            }
            None => {
                // Pixi pool empty — launch on demand via pixi exec (no pooled env needed)
                info!("[notebook-sync] Pixi pool empty, will launch on demand via pixi exec");
                return Some(None);
            }
        }
    }
    if parsed.package_manager() == Some(PackageManager::Conda) {
        match daemon.take_conda_env().await {
            Some(env) => {
                info!(
                    "[notebook-sync] Acquired Conda env from pool: {:?}",
                    env.python_path
                );
                Some(Some(env))
            }
            None => {
                error!("[notebook-sync] Conda pool empty, cannot launch");
                None // Signal caller to return early
            }
        }
    } else {
        // UV pool for uv:* sources and as default
        match daemon.take_uv_env().await {
            Some(env) => {
                info!(
                    "[notebook-sync] Acquired UV env from pool: {:?}",
                    env.python_path
                );
                Some(Some(env))
            }
            None => {
                error!("[notebook-sync] UV pool empty, cannot launch");
                None // Signal caller to return early
            }
        }
    }
}

/// Check if a notebook_id is a UUID (untitled/unsaved notebook).
pub(crate) fn is_untitled_notebook(notebook_id: &str) -> bool {
    uuid::Uuid::parse_str(notebook_id).is_ok()
}

/// Outcome the caller wants for `RuntimeStateDoc.kernel` after tearing
/// down a failed or stale runtime agent.
pub(crate) enum ResetOutcome<'a> {
    /// Clear the starting phase and leave the kernel in `NotStarted`. The
    /// frontend's auto-launch path can retry cleanly.
    NotStarted,
    /// Mark the kernel as `Error` and attach an error_details string the
    /// UI can render. `reason` is the typed key (or `None` for generic
    /// errors that don't fit the `KernelErrorReason` enum).
    Error {
        reason: Option<runtime_doc::KernelErrorReason>,
        details: &'a str,
    },
}

/// Reset runtime state after a failed or aborted launch. Writes the
/// requested outcome to `RuntimeStateDoc.kernel` and clears the stale
/// runtime agent handle / request channel / pending connect sender.
///
/// `expected_runtime_agent_id`: If `Some`, only reset if the current runtime agent
/// matches — prevents a stale error handler from clobbering a newer agent's state.
/// Pre-spawn callers pass `None` (no agent exists yet, always safe to reset).
pub(crate) async fn reset_starting_state_with_outcome<'a>(
    room: &NotebookRoom,
    expected_runtime_agent_id: Option<&str>,
    outcome: ResetOutcome<'a>,
) {
    // For guarded resets (post-spawn error paths), atomically check-and-clear
    // provenance AND capture the generation counter in a single write lock scope.
    // Clearing provenance to None immediately blocks late-arriving stale agents
    // because the connect handler rejects None provenance.
    //
    // The generation counter MUST be captured while holding the provenance write
    // lock. A new spawn acquires this same write lock (to set provenance) before
    // bumping the generation counter, so the generation cannot change while we
    // hold the lock. This closes the TOCTOU gap: if the generation changes after
    // we release the lock, the per-field checks (below) will detect the mismatch
    // and abort.
    let gen = if let Some(expected) = expected_runtime_agent_id {
        let mut current = room.current_runtime_agent_id.write().await;
        if current.as_deref() != Some(expected) {
            info!(
                "[notebook-sync] Skipping reset_starting_state: expected {} but current is {:?}",
                expected, *current,
            );
            return;
        }
        // Clear provenance and capture generation under the write lock.
        *current = None;
        Some(room.runtime_agent_generation.load(Ordering::Acquire))
    } else {
        None
    };

    // state handle uses std::sync::Mutex - no lock ordering concern
    // with runtime_agent_handle (tokio::sync::Mutex).
    if let Err(e) = room.state.with_doc(|sd| {
        match outcome {
            ResetOutcome::NotStarted => {
                sd.set_lifecycle(&RuntimeLifecycle::NotStarted)?;
            }
            ResetOutcome::Error { reason, details } => {
                sd.set_lifecycle_with_error_details(
                    &RuntimeLifecycle::Error,
                    reason,
                    Some(details),
                )?;
            }
        }
        sd.set_prewarmed_packages(&[])?;
        sd.clear_env_progress()?;
        Ok(())
    }) {
        warn!("[runtime-state] {}", e);
    }

    // Clear stale runtime agent handle so auto-launch can retry.
    // Check generation inside the lock: if a new spawn bumped it, the handle
    // belongs to the new generation — do not clear.
    {
        let mut guard = room.runtime_agent_handle.lock().await;
        if let Some(g) = gen {
            if room.runtime_agent_generation.load(Ordering::Acquire) != g {
                info!("[notebook-sync] Aborting reset_starting_state: new spawn detected at handle clear");
                return;
            }
        }
        *guard = None;
    }
    // Clear request channel — same generation guard.
    {
        let mut tx_guard = room.runtime_agent_request_tx.lock().await;
        if let Some(g) = gen {
            if room.runtime_agent_generation.load(Ordering::Acquire) != g {
                info!("[notebook-sync] Aborting reset_starting_state: new spawn detected at request_tx clear");
                return;
            }
        }
        *tx_guard = None;
    }
    // Clear pending connect sender — same generation guard.
    {
        let mut guard = room.pending_runtime_agent_connect_tx.lock().await;
        if let Some(g) = gen {
            if room.runtime_agent_generation.load(Ordering::Acquire) != g {
                info!("[notebook-sync] Aborting reset_starting_state: new spawn detected at connect_tx clear");
                return;
            }
        }
        *guard = None;
    }
}

/// Reset runtime state to `NotStarted`. Convenience wrapper around
/// [`reset_starting_state_with_outcome`] for call sites that don't have
/// a specific error message to surface — e.g. pool-empty, a user-side
/// config error the caller has already encoded in its own
/// `NotebookResponse::Error`, or a path that only needs to unwind the
/// "starting" state so auto-launch can retry.
pub(crate) async fn reset_starting_state(
    room: &NotebookRoom,
    expected_runtime_agent_id: Option<&str>,
) {
    reset_starting_state_with_outcome(room, expected_runtime_agent_id, ResetOutcome::NotStarted)
        .await;
}

/// Try to satisfy UV inline deps from the prewarmed pool.
///
/// Takes the pool env first, then compares against its *actual* `prewarmed_packages`
/// (not current settings) to avoid misclassifying stale pool entries.
/// Returns `Ok((PooledEnv, actual_packages))` on success, `Err(())` on failure.
pub(crate) async fn try_uv_pool_for_inline_deps(
    deps: &[String],
    bootstrap_dx: bool,
    daemon: &std::sync::Arc<crate::daemon::Daemon>,
    progress_handler: std::sync::Arc<dyn kernel_env::ProgressHandler>,
) -> Result<(crate::PooledEnv, Vec<String>), ()> {
    // `inline_deps_with_bootstrap` is a shim since 0.2.0 — launcher
    // vendoring replaced the `dx` PyPI install, so effective == inline.
    // Kept so the call sites don't all have to shift on the same PR.
    let effective_deps = crate::inline_env::inline_deps_with_bootstrap(deps, bootstrap_dx);

    // Quick pre-check: if any dep has version specifiers, skip pool entirely
    // (avoids consuming a pool env we'd have to discard)
    let settings_packages = daemon.uv_pool_packages().await;
    if matches!(
        crate::inline_env::compare_deps_to_pool(&effective_deps, &settings_packages),
        crate::inline_env::PoolDepRelation::Independent
    ) {
        debug!("[notebook-sync] UV inline deps have version constraints, skipping pool reuse");
        return Err(());
    }

    // Take the env, then compare against what it *actually* has installed
    let mut env = match daemon.take_uv_env().await {
        Some(env) => env,
        None => {
            info!("[notebook-sync] UV pool empty, falling back to full build");
            return Err(());
        }
    };
    let leased_path = env.venv_path.clone();

    let actual_packages = env.prewarmed_packages.clone();
    let relation = crate::inline_env::compare_deps_to_pool(&effective_deps, &actual_packages);

    match relation {
        crate::inline_env::PoolDepRelation::Subset => {
            info!("[notebook-sync] Inline UV deps are subset of pool env, reusing directly");
            // Promote the pool env into the inline-env cache so the next
            // restart with the same deps cache-hits instead of taking
            // another pool env. See #2089 / #2083.
            crate::inline_env::claim_pool_env_for_uv_inline_cache(
                &mut env,
                deps,
                None,
                bootstrap_dx,
            )
            .await;
            daemon
                .release_pool_lease(crate::EnvType::Uv, &leased_path)
                .await;
            Ok((env, actual_packages))
        }
        crate::inline_env::PoolDepRelation::Additive { delta } => {
            info!(
                "[notebook-sync] Inline UV deps are additive to pool env, delta: {:?}",
                delta
            );
            let uv_env = kernel_env::UvEnvironment {
                venv_path: env.venv_path.clone(),
                python_path: env.python_path.clone(),
            };
            progress_handler.on_progress(
                "uv",
                kernel_env::EnvProgressPhase::Installing { total: delta.len() },
            );
            match kernel_env::uv::sync_dependencies(&uv_env, &delta).await {
                Ok(()) => {
                    info!(
                        "[notebook-sync] Installed {} delta packages into pool env",
                        delta.len()
                    );
                    // Promote the pool env into the inline-env cache so
                    // the next restart cache-hits. See #2089 / #2083.
                    crate::inline_env::claim_pool_env_for_uv_inline_cache(
                        &mut env,
                        deps,
                        None,
                        bootstrap_dx,
                    )
                    .await;
                    daemon
                        .release_pool_lease(crate::EnvType::Uv, &leased_path)
                        .await;
                    progress_handler.on_progress(
                        "uv",
                        kernel_env::EnvProgressPhase::Ready {
                            env_path: env.venv_path.to_string_lossy().to_string(),
                            python_path: env.python_path.to_string_lossy().to_string(),
                        },
                    );
                    Ok((env, actual_packages))
                }
                Err(e) => {
                    warn!(
                        "[notebook-sync] Failed to install delta into UV pool env: {}, falling back",
                        e
                    );
                    // Clean up the taken pool env — it's out of the pool's
                    // tracking and would otherwise leak on disk.
                    daemon
                        .release_pool_lease(crate::EnvType::Uv, &leased_path)
                        .await;
                    let root = crate::paths::pool_env_root(&env.venv_path);
                    let cache_dir = crate::paths::default_cache_dir();
                    if crate::is_within_cache_dir(&root, &cache_dir) {
                        if let Err(e) = tokio::fs::remove_dir_all(&root).await {
                            warn!(
                                "[notebook-sync] Failed to clean up UV pool env {:?}: {}",
                                root, e
                            );
                        }
                    }
                    Err(())
                }
            }
        }
        crate::inline_env::PoolDepRelation::Independent => {
            // Shouldn't reach here (pre-check above), but handle gracefully
            debug!("[notebook-sync] UV pool env doesn't match inline deps, falling back");
            daemon
                .release_pool_lease(crate::EnvType::Uv, &leased_path)
                .await;
            let root = crate::paths::pool_env_root(&env.venv_path);
            let cache_dir = crate::paths::default_cache_dir();
            if crate::is_within_cache_dir(&root, &cache_dir) {
                if let Err(e) = tokio::fs::remove_dir_all(&root).await {
                    warn!(
                        "[notebook-sync] Failed to clean up UV pool env {:?}: {}",
                        root, e
                    );
                }
            }
            Err(())
        }
    }
}

/// Try to satisfy Conda inline deps from the prewarmed pool.
///
/// Only attempts pool reuse when channels are default (conda-forge).
/// Takes the pool env first, then compares against its *actual* `prewarmed_packages`.
/// Returns `Ok((PooledEnv, actual_packages))` on success, `Err(())` on failure.
pub(crate) async fn try_conda_pool_for_inline_deps(
    deps: &[String],
    channels: &[String],
    daemon: &std::sync::Arc<crate::daemon::Daemon>,
    progress_handler: std::sync::Arc<dyn kernel_env::ProgressHandler>,
) -> Result<(crate::PooledEnv, Vec<String>), ()> {
    // Only use pool for default conda-forge channel
    let is_default_channels =
        channels.is_empty() || (channels.len() == 1 && channels[0] == "conda-forge");
    if !is_default_channels {
        debug!(
            "[notebook-sync] Conda inline deps use non-default channels {:?}, skipping pool reuse",
            channels
        );
        return Err(());
    }

    // Quick pre-check: if any dep has version specifiers, skip pool entirely
    let settings_packages = daemon.conda_pool_packages().await;
    if matches!(
        crate::inline_env::compare_deps_to_pool(deps, &settings_packages),
        crate::inline_env::PoolDepRelation::Independent
    ) {
        debug!("[notebook-sync] Conda inline deps have version constraints, skipping pool reuse");
        return Err(());
    }

    // Take the env, then compare against what it *actually* has installed
    let mut env = match daemon.take_conda_env().await {
        Some(env) => env,
        None => {
            info!("[notebook-sync] Conda pool empty, falling back to full build");
            return Err(());
        }
    };
    let leased_path = env.venv_path.clone();

    let actual_packages = env.prewarmed_packages.clone();
    let relation = crate::inline_env::compare_deps_to_pool(deps, &actual_packages);

    match relation {
        crate::inline_env::PoolDepRelation::Subset => {
            info!("[notebook-sync] Inline Conda deps are subset of pool env, reusing directly");
            // Promote the pool env into the inline-env cache so the next
            // restart cache-hits. See #2089 / #2083.
            crate::inline_env::claim_pool_env_for_conda_inline_cache(&mut env, deps, channels)
                .await;
            daemon
                .release_pool_lease(crate::EnvType::Conda, &leased_path)
                .await;
            Ok((env, actual_packages))
        }
        crate::inline_env::PoolDepRelation::Additive { delta } => {
            info!(
                "[notebook-sync] Inline Conda deps are additive to pool env, delta: {:?}",
                delta
            );
            let conda_env = kernel_env::CondaEnvironment {
                env_path: env.venv_path.clone(),
                python_path: env.python_path.clone(),
            };
            // Pass ALL inline deps to sync_dependencies, not just the delta.
            // The conda solver treats the spec list as the complete desired
            // state: packages not in the specs get removed by the Installer.
            // Passing only the delta would drop the original user packages
            // that are already installed in the pool env. See #2134.
            let conda_deps = kernel_env::CondaDependencies {
                dependencies: deps.to_vec(),
                channels: vec!["conda-forge".to_string()],
                python: None,
                env_id: None,
            };
            progress_handler.on_progress(
                "conda",
                kernel_env::EnvProgressPhase::Installing { total: delta.len() },
            );
            match kernel_env::conda::sync_dependencies(&conda_env, &conda_deps).await {
                Ok(()) => {
                    info!(
                        "[notebook-sync] Installed {} delta packages into Conda pool env",
                        delta.len()
                    );
                    // Promote the pool env into the inline-env cache so
                    // the next restart cache-hits. See #2089 / #2083.
                    crate::inline_env::claim_pool_env_for_conda_inline_cache(
                        &mut env, deps, channels,
                    )
                    .await;
                    daemon
                        .release_pool_lease(crate::EnvType::Conda, &leased_path)
                        .await;
                    progress_handler.on_progress(
                        "conda",
                        kernel_env::EnvProgressPhase::Ready {
                            env_path: env.venv_path.to_string_lossy().to_string(),
                            python_path: env.python_path.to_string_lossy().to_string(),
                        },
                    );
                    Ok((env, actual_packages))
                }
                Err(e) => {
                    warn!(
                        "[notebook-sync] Failed to install delta into Conda pool env: {}, falling back",
                        e
                    );
                    daemon
                        .release_pool_lease(crate::EnvType::Conda, &leased_path)
                        .await;
                    let root = crate::paths::pool_env_root(&env.venv_path);
                    let cache_dir = crate::paths::default_cache_dir();
                    if crate::is_within_cache_dir(&root, &cache_dir) {
                        if let Err(e) = tokio::fs::remove_dir_all(&root).await {
                            warn!(
                                "[notebook-sync] Failed to clean up Conda pool env {:?}: {}",
                                root, e
                            );
                        }
                    }
                    Err(())
                }
            }
        }
        crate::inline_env::PoolDepRelation::Independent => {
            debug!("[notebook-sync] Conda pool env doesn't match inline deps, falling back");
            daemon
                .release_pool_lease(crate::EnvType::Conda, &leased_path)
                .await;
            let root = crate::paths::pool_env_root(&env.venv_path);
            let cache_dir = crate::paths::default_cache_dir();
            if crate::is_within_cache_dir(&root, &cache_dir) {
                if let Err(e) = tokio::fs::remove_dir_all(&root).await {
                    warn!(
                        "[notebook-sync] Failed to clean up Conda pool env {:?}: {}",
                        root, e
                    );
                }
            }
            Err(())
        }
    }
}

/// Auto-launch kernel for a trusted notebook when first peer connects.
/// This is similar to handle_notebook_request(LaunchKernel) but without a request/response.
///
/// Resolves the metadata snapshot from the Automerge doc (if the first client has
/// already synced) or falls back to reading the .ipynb from disk.
pub(crate) async fn auto_launch_kernel(
    room: &NotebookRoom,
    notebook_id: &str,
    default_runtime: crate::runtime::Runtime,
    default_python_env: crate::settings_doc::PythonEnvType,
    daemon: std::sync::Arc<crate::daemon::Daemon>,
) {
    // Check if room still has peers (protect against race condition where client disconnects
    // before we finish launching)
    if room
        .connections
        .active_peers
        .load(std::sync::atomic::Ordering::Relaxed)
        == 0
    {
        debug!("[notebook-sync] Auto-launch aborted: no peers remaining");
        reset_starting_state(room, None).await;
        return;
    }

    // For saved notebooks, notebook_path_opt is the file path (kernel cwd = parent dir).
    // For untitled notebooks, use working_dir as-is (output_prep handles is_dir()).
    let notebook_path = PathBuf::from(notebook_id);
    let notebook_path_opt = if notebook_path.exists() {
        Some(notebook_path.clone())
    } else if is_untitled_notebook(notebook_id) {
        let working_dir = room.identity.working_dir.read().await;
        working_dir.clone().inspect(|p| {
            info!(
                "[notebook-sync] Using working_dir for untitled notebook: {}",
                p.display()
            );
        })
    } else {
        None
    };

    // For project file detection, use the same path
    let working_dir_for_detection = notebook_path_opt.clone();

    // Resolve metadata snapshot: try Automerge doc first, fall back to disk
    let metadata_snapshot = resolve_metadata_snapshot(room, notebook_path_opt.as_deref()).await;

    // Check RuntimeStateDoc — skip if already starting or running
    {
        // Skip if runtime agent already exists (another auto-launch won the race)
        // or kernel is already running
        let has_runtime_agent = room.runtime_agent_handle.lock().await.is_some();
        if has_runtime_agent {
            debug!("[notebook-sync] Auto-launch skipped: runtime agent already exists");
            return;
        }
    }

    // Re-check peers (another race check)
    if room
        .connections
        .active_peers
        .load(std::sync::atomic::Ordering::Relaxed)
        == 0
    {
        debug!("[notebook-sync] Auto-launch aborted: no peers (after status check)");
        reset_starting_state(room, None).await;
        return;
    }

    // Clear any stale comm state from a previous kernel (in case it crashed)
    if let Err(e) = room.state.with_doc(|sd| {
        sd.clear_comms()?;
        Ok(())
    }) {
        warn!("[runtime-state] {}", e);
    }

    // Detection priority:
    // 1. Notebook's kernelspec (for existing notebooks) - determines python vs deno
    // 2. For Python: resolve environment (inline deps → project files → prewarmed)
    // 3. For Deno: just launch Deno (no env resolution needed)
    // 4. For new notebooks (no kernelspec): use default_runtime setting

    // Step 1: Detect kernel type from metadata snapshot
    let notebook_kernel_type = metadata_snapshot.as_ref().and_then(|s| s.detect_runtime());

    // Step 2a: If the notebook has a captured prewarmed env on disk, route
    // back through the prewarmed capture path so the reopen cache-hit fires.
    // This overrides inline-deps detection because captured deps are
    // structurally indistinguishable from user-authored inline deps.
    let (captured_override, captured_env_for_config) =
        resolve_captured_env_override(metadata_snapshot.as_ref());
    if let Some(ref src) = captured_override {
        info!(
            "[notebook-sync] Auto-launch: captured env on disk -> {}",
            src.as_str()
        );
    }

    // Step 2b: Check inline deps (for environment source, and runt.deno override)
    let inline_source: Option<notebook_protocol::connection::EnvSource> = captured_override
        .clone()
        .or_else(|| metadata_snapshot.as_ref().and_then(check_inline_deps));

    // Step 2b: If no metadata inline deps, check cell source for PEP 723 script blocks
    let (inline_source, pep723_deps) = if inline_source.is_some() {
        (inline_source, None)
    } else {
        let cells = room.doc.read().await.get_cells();
        match notebook_doc::pep723::find_pep723_in_cells(&cells) {
            Ok(Some(meta)) if !meta.dependencies.is_empty() => {
                use notebook_protocol::connection::{EnvSource, PackageManager};
                // Route PEP 723 deps based on user's default Python env
                let pep723_source = match default_python_env {
                    crate::settings_doc::PythonEnvType::Pixi => {
                        EnvSource::Pep723(PackageManager::Pixi)
                    }
                    _ => EnvSource::Pep723(PackageManager::Uv),
                };
                info!(
                    "[notebook-sync] Auto-launch: found PEP 723 deps ({}) : {:?}",
                    pep723_source.as_str(),
                    meta.dependencies
                );
                (Some(pep723_source), Some(meta.dependencies))
            }
            Ok(_) => (None, None),
            Err(e) => {
                warn!("[notebook-sync] PEP 723 parse error: {}", e);
                (None, None)
            }
        }
    };

    // Step 3: Check project files (for Python environment resolution)
    // Use notebook path for saved notebooks, or working_dir for untitled notebooks
    let detection_path = notebook_path_opt
        .as_ref()
        .or(working_dir_for_detection.as_ref());
    let detected_project_file =
        detection_path.and_then(|path| crate::project_file::detect_project_file(path));
    if let Some(ref detected) = detected_project_file {
        info!(
            "[notebook-sync] Auto-launch: detected project file {:?} -> {}",
            detected.path,
            detected.to_env_source().as_str()
        );
    }
    let project_source: Option<notebook_protocol::connection::EnvSource> =
        detected_project_file.as_ref().map(|d| d.to_env_source());

    // #2157/#2170: If the detected project file is an environment.yml whose
    // declared conda env isn't built on this machine, surface an explicit
    // awaiting-user-decision lifecycle instead of spawning a runtime agent that would die with
    // "could not resolve conda environment prefix". Silent fallback to
    // a pool env was rejected as user-hostile: env.yml users expect
    // their declared deps to be installed, and a pool env lacks them.
    // Writing AwaitingEnvBuild + details gives the frontend enough to
    // render a cohesive decision dialog and MCP tools enough to report the miss.
    if let Some(ref detected) = detected_project_file {
        if let Some(decision) = missing_conda_env_yml_decision(detected) {
            if project_environment_build_approved(room, detected) {
                info!(
                    "[notebook-sync] Approved environment.yml build for {:?}; continuing launch",
                    detected.path
                );
            } else {
                let details = format_conda_env_yml_build_details(&decision);
                warn!("[notebook-sync] {}", details);
                if let Err(e) = room.state.with_doc(|sd| {
                    sd.set_lifecycle_with_error_details(
                        &RuntimeLifecycle::AwaitingEnvBuild,
                        Some(runtime_doc::KernelErrorReason::CondaEnvYmlMissing),
                        Some(&details),
                    )
                }) {
                    warn!("[runtime-state] {}", e);
                }
                return;
            }
        }
    }

    // Determine kernel type and environment
    use notebook_protocol::connection::{EnvSource, PackageManager};
    let (kernel_type, env_source, pooled_env): (&str, EnvSource, Option<crate::PooledEnv>) =
        match notebook_kernel_type.as_deref() {
            Some("deno") => {
                // Notebook is a Deno notebook (per its kernelspec)
                info!("[notebook-sync] Auto-launch: Deno kernel (notebook kernelspec)");
                ("deno", EnvSource::Deno, None)
            }
            Some("python") => {
                // Notebook is a Python notebook - resolve environment
                // Priority: project file > inline deps > prewarmed
                // Project file wins because inline deps get promoted to the
                // project file at sync/launch time (project is source of truth).
                let env_source: EnvSource = if let Some(ref proj) = project_source {
                    info!(
                        "[notebook-sync] Auto-launch: using project file -> {}",
                        proj.as_str()
                    );
                    proj.clone()
                } else if let Some(ref source) = inline_source {
                    // Skip Deno inline source for Python notebooks (kernelspec takes priority)
                    if !matches!(source, EnvSource::Deno) {
                        info!(
                            "[notebook-sync] Auto-launch: found inline deps -> {}",
                            source.as_str()
                        );
                        source.clone()
                    } else {
                        EnvSource::Prewarmed(default_prewarmed_manager(default_python_env.clone()))
                    }
                } else {
                    // Check if the metadata has an explicit manager section
                    // (e.g. create_notebook(package_manager="conda") with empty deps).
                    // Use that to pick the pool type instead of default_python_env.
                    let manager = metadata_snapshot
                        .as_ref()
                        .and_then(detect_manager_from_metadata)
                        .and_then(|pm| match pm {
                            PackageManager::Unknown(_) => None,
                            canonical => Some(canonical),
                        })
                        .unwrap_or_else(|| default_prewarmed_manager(default_python_env.clone()));
                    let prewarmed = EnvSource::Prewarmed(manager);
                    info!(
                        "[notebook-sync] Auto-launch: using prewarmed ({})",
                        prewarmed.as_str()
                    );
                    prewarmed
                };
                let pooled_env = if env_source.prepares_own_env() {
                    info!(
                        "[notebook-sync] Auto-launch: {} prepares its own env, no pool env needed",
                        env_source.as_str()
                    );
                    None
                } else {
                    match acquire_prewarmed_env_with_capture(
                        env_source.as_str(),
                        &daemon,
                        room,
                        metadata_snapshot.as_ref(),
                    )
                    .await
                    {
                        Some(env) => env,
                        None => {
                            reset_starting_state(room, None).await;
                            return;
                        }
                    }
                };
                ("python", env_source, pooled_env)
            }
            None => {
                // New notebook or unknown kernelspec - use default_runtime
                if matches!(inline_source, Some(EnvSource::Deno)) {
                    // runt.deno config present
                    info!("[notebook-sync] Auto-launch: Deno kernel (runt.deno config)");
                    ("deno", EnvSource::Deno, None)
                } else if matches!(default_runtime, crate::runtime::Runtime::Deno) {
                    // User's default is Deno
                    info!("[notebook-sync] Auto-launch: Deno kernel (default runtime)");
                    ("deno", EnvSource::Deno, None)
                } else {
                    // Default to Python
                    // Priority: project file > inline deps > prewarmed
                    let env_source: EnvSource = if let Some(ref source) = project_source {
                        info!(
                            "[notebook-sync] Auto-launch: using project file -> {}",
                            source.as_str()
                        );
                        source.clone()
                    } else if let Some(ref source) = inline_source {
                        info!(
                            "[notebook-sync] Auto-launch: found inline deps -> {}",
                            source.as_str()
                        );
                        source.clone()
                    } else {
                        let prewarmed = EnvSource::Prewarmed(default_prewarmed_manager(
                            default_python_env.clone(),
                        ));
                        info!(
                            "[notebook-sync] Auto-launch: using prewarmed ({})",
                            prewarmed.as_str()
                        );
                        prewarmed
                    };
                    let pooled_env = if env_source.prepares_own_env() {
                        info!(
                            "[notebook-sync] Auto-launch: {} prepares its own env, no pool env needed",
                            env_source.as_str()
                        );
                        None
                    } else {
                        match acquire_prewarmed_env_with_capture(
                            env_source.as_str(),
                            &daemon,
                            room,
                            metadata_snapshot.as_ref(),
                        )
                        .await
                        {
                            Some(env) => env,
                            None => {
                                reset_starting_state(room, None).await;
                                return;
                            }
                        }
                    };
                    ("python", env_source, pooled_env)
                }
            }
            Some(other) => {
                // Unknown kernel type - default to Python
                warn!(
                    "[notebook-sync] Unknown kernel type '{}', defaulting to Python",
                    other
                );
                let prewarmed =
                    EnvSource::Prewarmed(default_prewarmed_manager(default_python_env.clone()));
                let pooled_env = match acquire_prewarmed_env_with_capture(
                    prewarmed.as_str(),
                    &daemon,
                    room,
                    metadata_snapshot.as_ref(),
                )
                .await
                {
                    Some(env) => env,
                    None => {
                        reset_starting_state(room, None).await;
                        return;
                    }
                };
                ("python", prewarmed, pooled_env)
            }
        };

    // For pixi:toml, verify ipykernel is in pixi.toml before launching.
    // Unlike uv (`uv run --with ipykernel`) and conda:env_yml (daemon
    // appends ipykernel into the dep set pre-sync), pixi does not
    // auto-inject — the project file is the sole source of truth, so
    // launching without ipykernel would spawn a Python process that
    // fails on `ipykernel_launcher` import.
    if matches!(env_source, EnvSource::PixiToml) {
        if let Some(ref detected) = detected_project_file {
            let has_ipykernel = match kernel_launch::tools::pixi_info(&detected.path).await {
                Ok(info) => info.has_ipykernel(),
                Err(e) => {
                    warn!(
                        "[notebook-sync] pixi info failed, falling back to line scan: {}",
                        e
                    );
                    crate::project_file::pixi_toml_has_ipykernel(&detected.path)
                }
            };
            if !has_ipykernel {
                warn!(
                    "[notebook-sync] pixi.toml at {:?} does not declare ipykernel — cannot launch kernel",
                    detected.path
                );
                if let Err(e) = room.state.with_doc(|sd| {
                    sd.set_lifecycle_with_error(
                        &RuntimeLifecycle::Error,
                        Some(KernelErrorReason::MissingIpykernel),
                    )?;
                    sd.set_kernel_info("python", "python", env_source.as_str())?;
                    Ok(())
                }) {
                    warn!("[runtime-state] {}", e);
                }
                return;
            }
        }
    }

    // Transition to PreparingEnv now that runtime/env has been resolved.
    if let Err(e) = room
        .state
        .with_doc(|sd| sd.set_lifecycle(&RuntimeLifecycle::PreparingEnv))
    {
        warn!("[runtime-state] {}", e);
    }

    // For inline deps, prepare a cached environment with rich progress
    let progress_handler: std::sync::Arc<dyn kernel_env::ProgressHandler> =
        std::sync::Arc::new(crate::inline_env::BroadcastProgressHandler::with_state(
            room.broadcasts.kernel_broadcast_tx.clone(),
            room.state.clone(),
        ));

    // Fetch feature flags now so inline env prep hashes match what the
    // kernel will actually receive (bootstrap_dx changes the install set).
    let feature_flags_for_inline = daemon.feature_flags().await;
    let bootstrap_dx = feature_flags_for_inline.bootstrap_dx;

    let (pooled_env, inline_deps) = if matches!(env_source, EnvSource::Pep723(PackageManager::Uv)) {
        // PEP 723 deps were extracted from cell source in step 2b
        if let Some(ref deps) = pep723_deps {
            info!(
                "[notebook-sync] Preparing cached UV env for PEP 723 deps: {:?}",
                deps
            );
            match crate::inline_env::prepare_uv_inline_env(
                deps,
                None,
                progress_handler.clone(),
                bootstrap_dx,
            )
            .await
            {
                Ok(prepared) => {
                    info!(
                        "[notebook-sync] Using cached PEP 723 env at {:?}",
                        prepared.python_path
                    );
                    let env = Some(crate::PooledEnv {
                        env_type: crate::EnvType::Uv,
                        venv_path: prepared.env_path,
                        python_path: prepared.python_path,
                        prewarmed_packages: vec![],
                    });
                    (env, Some(deps.clone()))
                }
                Err(e) => {
                    error!("[notebook-sync] Failed to prepare PEP 723 env: {}", e);
                    reset_starting_state(room, None).await;
                    return;
                }
            }
        } else {
            (None, None)
        }
    } else if matches!(env_source, EnvSource::Inline(PackageManager::Uv)) {
        if let Some(deps) = metadata_snapshot.as_ref().and_then(get_inline_uv_deps) {
            let prerelease = metadata_snapshot
                .as_ref()
                .and_then(get_inline_uv_prerelease);

            // Fast path: check inline env cache first (instant on hit).
            // `check_uv_inline_cache` re-vendors the launcher on hit when
            // bootstrap_dx is on, so a stale pre-0.2.0 cache entry is
            // brought up to today's layout before the kernel boots.
            if let Some(cached) =
                crate::inline_env::check_uv_inline_cache(&deps, prerelease.as_deref(), bootstrap_dx)
                    .await
            {
                info!(
                    "[notebook-sync] UV inline cache hit at {:?}",
                    cached.python_path
                );
                let env = Some(crate::PooledEnv {
                    env_type: crate::EnvType::Uv,
                    venv_path: cached.env_path,
                    python_path: cached.python_path,
                    prewarmed_packages: vec![],
                });
                (env, Some(deps))
            } else if prerelease.is_none() {
                // Try pool reuse for bare deps without prerelease
                match try_uv_pool_for_inline_deps(
                    &deps,
                    bootstrap_dx,
                    &daemon,
                    progress_handler.clone(),
                )
                .await
                {
                    Ok((env, pool_pkgs)) => {
                        let mut pooled = env;
                        pooled.prewarmed_packages = pool_pkgs;
                        (Some(pooled), Some(deps))
                    }
                    Err(_) => {
                        // Pool path failed, fall back to full build
                        info!(
                            "[notebook-sync] Preparing cached UV env for inline deps: {:?}",
                            deps
                        );
                        match crate::inline_env::prepare_uv_inline_env(
                            &deps,
                            prerelease.as_deref(),
                            progress_handler.clone(),
                            bootstrap_dx,
                        )
                        .await
                        {
                            Ok(prepared) => {
                                info!(
                                    "[notebook-sync] Using cached inline env at {:?}",
                                    prepared.python_path
                                );
                                let env = Some(crate::PooledEnv {
                                    env_type: crate::EnvType::Uv,
                                    venv_path: prepared.env_path,
                                    python_path: prepared.python_path,
                                    prewarmed_packages: vec![],
                                });
                                (env, Some(deps))
                            }
                            Err(e) => {
                                error!("[notebook-sync] Failed to prepare inline env: {}", e);
                                reset_starting_state(room, None).await;
                                return;
                            }
                        }
                    }
                }
            } else {
                // Has prerelease — can't use pool, go straight to full build
                info!(
                    "[notebook-sync] Preparing cached UV env for inline deps: {:?} (prerelease: {:?})",
                    deps, prerelease
                );
                match crate::inline_env::prepare_uv_inline_env(
                    &deps,
                    prerelease.as_deref(),
                    progress_handler.clone(),
                    bootstrap_dx,
                )
                .await
                {
                    Ok(prepared) => {
                        info!(
                            "[notebook-sync] Using cached inline env at {:?}",
                            prepared.python_path
                        );
                        let env = Some(crate::PooledEnv {
                            env_type: crate::EnvType::Uv,
                            venv_path: prepared.env_path,
                            python_path: prepared.python_path,
                            prewarmed_packages: vec![],
                        });
                        (env, Some(deps))
                    }
                    Err(e) => {
                        error!("[notebook-sync] Failed to prepare inline env: {}", e);
                        reset_starting_state(room, None).await;
                        return;
                    }
                }
            }
        } else {
            (pooled_env, None)
        }
    } else if matches!(env_source, EnvSource::Inline(PackageManager::Conda)) {
        if let Some(deps) = metadata_snapshot.as_ref().and_then(get_inline_conda_deps) {
            let channels = metadata_snapshot
                .as_ref()
                .map(get_inline_conda_channels)
                .unwrap_or_else(|| vec!["conda-forge".to_string()]);

            // Fast path: check inline env cache first (instant on hit)
            if let Some(cached) = crate::inline_env::check_conda_inline_cache(&deps, &channels) {
                info!(
                    "[notebook-sync] Conda inline cache hit at {:?}",
                    cached.python_path
                );
                let env = Some(crate::PooledEnv {
                    env_type: crate::EnvType::Conda,
                    venv_path: cached.env_path,
                    python_path: cached.python_path,
                    prewarmed_packages: vec![],
                });
                (env, Some(deps))
            } else {
                // Try pool reuse (only for default conda-forge channel)
                match try_conda_pool_for_inline_deps(
                    &deps,
                    &channels,
                    &daemon,
                    progress_handler.clone(),
                )
                .await
                {
                    Ok((env, pool_pkgs)) => {
                        let mut pooled = env;
                        pooled.prewarmed_packages = pool_pkgs;
                        (Some(pooled), Some(deps))
                    }
                    Err(_) => {
                        // Pool path failed, fall back to full build
                        info!(
                            "[notebook-sync] Preparing cached Conda env for inline deps: {:?} (channels: {:?})",
                            deps, channels
                        );
                        match crate::inline_env::prepare_conda_inline_env(
                            &deps,
                            &channels,
                            progress_handler.clone(),
                        )
                        .await
                        {
                            Ok(prepared) => {
                                info!(
                                    "[notebook-sync] Using cached conda inline env at {:?}",
                                    prepared.python_path
                                );
                                let env = Some(crate::PooledEnv {
                                    env_type: crate::EnvType::Conda,
                                    venv_path: prepared.env_path,
                                    python_path: prepared.python_path,
                                    prewarmed_packages: vec![],
                                });
                                (env, Some(deps))
                            }
                            Err(e) => {
                                error!("[notebook-sync] Failed to prepare conda inline env: {}", e);
                                reset_starting_state(room, None).await;
                                return;
                            }
                        }
                    }
                }
            }
        } else {
            (pooled_env, None)
        }
    } else if matches!(env_source, EnvSource::EnvYml) {
        let yml_path = detected_project_file
            .as_ref()
            .filter(|d| d.kind == crate::project_file::ProjectFileKind::EnvironmentYml)
            .map(|d| d.path.clone())
            .or_else(|| {
                notebook_path_opt.as_ref().and_then(|p| {
                    crate::project_file::find_nearest_project_file(
                        p,
                        &[crate::project_file::ProjectFileKind::EnvironmentYml],
                    )
                    .map(|d| d.path)
                })
            });

        let Some(yml_path) = yml_path else {
            warn!("[notebook-sync] conda:env_yml but no environment.yml found");
            reset_starting_state(room, None).await;
            return;
        };

        let detected_yml = crate::project_file::DetectedProjectFile {
            path: yml_path.clone(),
            kind: crate::project_file::ProjectFileKind::EnvironmentYml,
        };
        if let Some(decision) = missing_conda_env_yml_decision(&detected_yml) {
            if project_environment_build_approved(room, &detected_yml) {
                info!(
                    "[notebook-sync] Approved environment.yml build for {:?}; continuing launch",
                    detected_yml.path
                );
            } else {
                let details = format_conda_env_yml_build_details(&decision);
                warn!("[notebook-sync] {}", details);
                if let Err(e) = room.state.with_doc(|sd| {
                    sd.set_lifecycle_with_error_details(
                        &RuntimeLifecycle::AwaitingEnvBuild,
                        Some(KernelErrorReason::CondaEnvYmlMissing),
                        Some(&details),
                    )
                }) {
                    warn!("[runtime-state] {}", e);
                }
                return;
            }
        }

        let env_config = match crate::project_file::parse_environment_yml(&yml_path) {
            Ok(config) => config,
            Err(e) => {
                error!("[notebook-sync] Failed to parse environment.yml: {}", e);
                reset_starting_state(room, None).await;
                return;
            }
        };

        let conda_prefix = if let Some(ref prefix) = env_config.prefix {
            prefix.clone()
        } else if let Some(ref name) = env_config.name {
            crate::project_file::find_named_conda_env(name)
                .unwrap_or_else(|| crate::project_file::default_conda_envs_dir().join(name))
        } else {
            let cache_dir = crate::paths::default_cache_dir().join("conda-envs");
            let conda_deps_tmp = kernel_env::CondaDependencies {
                dependencies: env_config.dependencies.clone(),
                channels: env_config.channels.clone(),
                python: env_config.python.clone(),
                env_id: None,
            };
            cache_dir.join(kernel_env::conda::compute_env_hash(&conda_deps_tmp))
        };

        let mut all_deps = env_config.dependencies.clone();
        if let Some(crdt_deps) = metadata_snapshot.as_ref().and_then(get_inline_conda_deps) {
            let base_names: std::collections::HashSet<String> = all_deps
                .iter()
                .map(|d| notebook_doc::metadata::extract_package_name(d).to_lowercase())
                .collect();
            for dep in &crdt_deps {
                let name = notebook_doc::metadata::extract_package_name(dep).to_lowercase();
                if !base_names.contains(&name) {
                    all_deps.push(dep.clone());
                }
            }
        }

        let base_names: std::collections::HashSet<String> = all_deps
            .iter()
            .map(|d| notebook_doc::metadata::extract_package_name(d).to_lowercase())
            .collect();
        if !base_names.contains("ipykernel") {
            all_deps.push("ipykernel".to_string());
        }

        let channels = if env_config.channels.is_empty() {
            vec!["conda-forge".to_string()]
        } else {
            env_config.channels.clone()
        };

        let env_name_display = env_config.name.as_deref().unwrap_or("<unnamed>");
        info!(
            "[notebook-sync] conda:env_yml: env '{}' at {:?} with {} deps",
            env_name_display,
            conda_prefix,
            all_deps.len()
        );

        let conda_deps = kernel_env::CondaDependencies {
            dependencies: all_deps,
            channels,
            python: env_config.python.clone(),
            env_id: None,
        };

        let python_path = crate::project_file::conda_python_path(&conda_prefix);
        if python_path.exists() {
            let conda_env = kernel_env::CondaEnvironment {
                env_path: conda_prefix.clone(),
                python_path: python_path.clone(),
            };
            progress_handler.on_progress(
                "conda",
                kernel_env::EnvProgressPhase::Installing {
                    total: conda_deps.dependencies.len(),
                },
            );
            if let Err(e) = kernel_env::conda::sync_dependencies(&conda_env, &conda_deps).await {
                warn!(
                    "[notebook-sync] conda:env_yml sync into existing env failed: {}, continuing with existing env",
                    e
                );
            }
            let env = Some(crate::PooledEnv {
                env_type: crate::EnvType::Conda,
                venv_path: conda_prefix,
                python_path,
                prewarmed_packages: vec![],
            });
            (
                env,
                metadata_snapshot.as_ref().and_then(get_inline_conda_deps),
            )
        } else {
            let parent = conda_prefix
                .parent()
                .unwrap_or_else(|| std::path::Path::new("/tmp"));
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                error!(
                    "[notebook-sync] Failed to create conda envs directory {:?}: {}",
                    parent, e
                );
                reset_starting_state(room, None).await;
                return;
            }

            match kernel_env::conda::prepare_environment_in(
                &conda_deps,
                parent,
                progress_handler.clone(),
            )
            .await
            {
                Ok(prepared) => {
                    let final_prefix = if prepared.env_path != conda_prefix {
                        match tokio::fs::rename(&prepared.env_path, &conda_prefix).await {
                            Ok(()) => conda_prefix.clone(),
                            Err(e) => {
                                warn!(
                                    "[notebook-sync] Failed to rename {:?} -> {:?}: {}, using hash path",
                                    prepared.env_path, conda_prefix, e
                                );
                                prepared.env_path
                            }
                        }
                    } else {
                        prepared.env_path
                    };
                    let python = crate::project_file::conda_python_path(&final_prefix);
                    let env = Some(crate::PooledEnv {
                        env_type: crate::EnvType::Conda,
                        venv_path: final_prefix,
                        python_path: python,
                        prewarmed_packages: vec![],
                    });
                    (
                        env,
                        metadata_snapshot.as_ref().and_then(get_inline_conda_deps),
                    )
                }
                Err(e) => {
                    error!(
                        "[notebook-sync] Failed to create conda env '{}' from environment.yml: {}",
                        env_name_display, e
                    );
                    reset_starting_state(room, None).await;
                    return;
                }
            }
        }
    } else if matches!(env_source, EnvSource::Inline(PackageManager::Pixi)) {
        // pixi exec handles its own env caching — just extract deps for the -w flags
        let deps = metadata_snapshot
            .as_ref()
            .and_then(|s| s.runt.pixi.as_ref())
            .map(|p| p.dependencies.clone())
            .unwrap_or_default();
        if !deps.is_empty() {
            info!("[notebook-sync] pixi:inline deps for pixi exec: {:?}", deps);
            (None, Some(deps))
        } else {
            (pooled_env, None)
        }
    } else if matches!(env_source, EnvSource::Pep723(PackageManager::Pixi)) {
        // PEP 723 deps via pixi exec -w (same mechanism as pixi:inline)
        if let Some(ref deps) = pep723_deps {
            info!("[notebook-sync] pixi:pep723 deps for pixi exec: {:?}", deps);
            (None, Some(deps.clone()))
        } else {
            (pooled_env, None)
        }
    } else {
        (pooled_env, None)
    };

    // Verify ipykernel is actually present in the prepared env before we
    // spawn the kernel. For UV/Conda inline + PEP 723, `prepare_environment_in`
    // always adds `ipykernel` to the install set, but cache hits skip the
    // install step and re-use whatever's on disk. Stale caches (pre-base-
    // packages daemon builds, hand-edited venvs, partial installs) can slip
    // through and then fail at kernel spawn with a generic
    // `ModuleNotFoundError: ipykernel_launcher`. Gating here surfaces the
    // typed `MissingIpykernel` reason so the UI can render env-specific
    // remediation instead of a raw stderr dump.
    //
    // Skipped for:
    // - Prewarmed pool envs (daemon-managed; base packages are invariant)
    // - `pixi:inline` / `pixi:pep723` (no pooled_env, pixi exec handles it)
    // - `uv:pyproject` (launched via `uv run --with ipykernel` — auto-injects)
    // - `conda:env_yml` (daemon appends ipykernel to the dep set pre-sync)
    // - `deno` (not a Python env)
    // - `pixi:toml` (already gated above via `pixi_info().has_ipykernel()`)
    if matches!(
        env_source,
        EnvSource::Inline(PackageManager::Uv)
            | EnvSource::Inline(PackageManager::Conda)
            | EnvSource::Pep723(PackageManager::Uv)
    ) {
        if let Some(ref env) = pooled_env {
            if !kernel_env::venv_has_ipykernel(&env.python_path) {
                warn!(
                    "[notebook-sync] prepared env at {:?} ({}) is missing ipykernel — cannot launch kernel",
                    env.venv_path,
                    env_source.as_str()
                );
                // Don't delete the env dir here: it's a content-addressed
                // cache shared with any other notebook using the same
                // dep set, and there's a race where a concurrent launch
                // could observe a partial build (python present,
                // ipykernel not yet installed) and nuke the env out
                // from under the in-progress installer. A proper heal
                // (install ipykernel in place, or atomic invalidation
                // marker) is a follow-up; for now we surface the typed
                // error and let the user edit deps to bump the hash.
                let env_source_label = env_source.as_str().to_string();
                if let Err(e) = room.state.with_doc(|sd| {
                    sd.set_lifecycle_with_error(
                        &RuntimeLifecycle::Error,
                        Some(KernelErrorReason::MissingIpykernel),
                    )?;
                    sd.set_kernel_info("python", "python", &env_source_label)?;
                    Ok(())
                }) {
                    warn!("[runtime-state] {}", e);
                }
                return;
            }
        }
    }

    // Register the env path for GC protection immediately after pool.take(),
    // BEFORE any async work (agent spawn, connect timeout, delta install).
    // Without this, there's a race window where GC sees the taken env as an
    // orphan and deletes it while we're still setting up the kernel.
    if let Some(ref env) = pooled_env {
        {
            let mut ep = room.runtime_agent_env_path.write().await;
            *ep = Some(env.venv_path.clone());
        }
        daemon
            .release_pool_lease(env.env_type, &env.venv_path)
            .await;
    }

    // Build LaunchedEnvConfig to track what config the kernel was launched with
    let venv_path = pooled_env.as_ref().map(|e| e.venv_path.clone());
    let python_path = pooled_env.as_ref().map(|e| e.python_path.clone());
    let prewarmed_pkgs = if let Some(ref env) = pooled_env {
        Some(env.prewarmed_packages.clone())
    } else if env_source.package_manager() == Some(PackageManager::Pixi) {
        // For pixi launches without a pooled env, read default packages from settings
        let pixi_defaults = daemon.default_pixi_packages().await;
        if pixi_defaults.is_empty() {
            None
        } else {
            Some(pixi_defaults)
        }
    } else {
        None
    };
    let feature_flags = feature_flags_for_inline;
    // Pass captured env only when this launch actually routed through the
    // captured path. If the capture data is present but env_source flipped
    // to a different family (auto:uv vs captured conda, project file wins,
    // etc.), `captured_env_for_config` stays None so drift detection
    // doesn't get misleading deps from the wrong runtime.
    let captured_for_config = captured_env_for_config.as_ref().filter(|c| match c {
        CapturedEnv::Uv { .. } => matches!(env_source, EnvSource::Prewarmed(PackageManager::Uv)),
        CapturedEnv::Conda { .. } => {
            matches!(env_source, EnvSource::Prewarmed(PackageManager::Conda))
        }
    });
    let launched_config = build_launched_config(
        kernel_type,
        env_source.as_str(),
        inline_deps.as_deref(),
        metadata_snapshot.as_ref(),
        venv_path,
        python_path,
        prewarmed_pkgs.as_deref(),
        notebook_path_opt.as_deref(),
        feature_flags,
        captured_for_config,
    );

    // Transition to "launching" phase before starting the kernel process
    if let Err(e) = room
        .state
        .with_doc(|sd| sd.set_lifecycle(&RuntimeLifecycle::Launching))
    {
        warn!("[runtime-state] {}", e);
    }

    // (prewarmed_packages no longer needed — runtime agent handles its own launch config)

    // Spawn runtime agent subprocess for kernel execution
    {
        info!("[notebook-sync] Spawning runtime agent subprocess for auto-launch");

        // Always pass the room UUID so the agent's RuntimeAgent handshake
        // finds the room in the UUID-keyed rooms map.
        let nb_id = room.id.to_string();
        let runtime_agent_id = format!("runtime-agent:{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let socket_path = daemon.socket_path().clone();

        // Set provenance BEFORE spawn so stale agents are rejected by the
        // connect handler's provenance check. Bump generation BEFORE storing
        // the oneshot so reset_starting_state can detect interleaving spawns.
        // Then create the oneshot so it's ready before the subprocess can
        // connect.
        //
        // Ordering: provenance → generation → oneshot → spawn
        {
            let mut id = room.current_runtime_agent_id.write().await;
            *id = Some(runtime_agent_id.clone());
        }
        room.runtime_agent_generation
            .fetch_add(1, Ordering::Release);
        let runtime_agent_connect_rx = {
            let (tx, rx) = oneshot::channel();
            let mut guard = room.pending_runtime_agent_connect_tx.lock().await;
            *guard = Some(tx);
            rx
        };

        match crate::runtime_agent_handle::RuntimeAgentHandle::spawn(
            nb_id,
            runtime_agent_id.clone(),
            room.blob_store.root().to_path_buf(),
            socket_path,
        )
        .await
        {
            Ok(ra) => {
                // Store handle after spawn succeeds.
                {
                    let mut ra_guard = room.runtime_agent_handle.lock().await;
                    *ra_guard = Some(ra);
                }

                // Connecting lifecycle — fills the gap between spawn and connect
                if let Err(e) = room
                    .state
                    .with_doc(|sd| sd.set_lifecycle(&RuntimeLifecycle::Connecting))
                {
                    warn!("[runtime-state] {}", e);
                }

                // Wait for THIS runtime agent to establish its sync connection
                match tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    runtime_agent_connect_rx,
                )
                .await
                {
                    Ok(Ok(())) => {
                        info!("[notebook-sync] Agent connected, sending LaunchKernel");
                    }
                    Ok(Err(_)) => {
                        // Oneshot sender dropped — runtime agent died or was
                        // superseded by a newer spawn before connecting.
                        warn!(
                            "[notebook-sync] Runtime agent connect cancelled (superseded or died)"
                        );
                        reset_starting_state(room, Some(&runtime_agent_id)).await;
                        return;
                    }
                    Err(_) => {
                        warn!("[notebook-sync] Agent failed to connect within 30s");
                        reset_starting_state(room, Some(&runtime_agent_id)).await;
                        return;
                    }
                }

                // Send LaunchKernel RPC via the runtime agent's sync connection
                match send_runtime_agent_request_with_kernel_ports(room, |kernel_ports| {
                    notebook_protocol::protocol::RuntimeAgentRequest::LaunchKernel {
                        kernel_type: kernel_type.to_string(),
                        env_source: env_source.clone(),
                        notebook_path: notebook_path_opt
                            .as_deref()
                            .map(|p| p.to_str().unwrap_or("").to_string()),
                        launched_config: launched_config.clone(),
                        kernel_ports,
                        env_vars: Default::default(),
                    }
                })
                .await
                {
                    Ok(notebook_protocol::protocol::RuntimeAgentResponse::KernelLaunched {
                        env_source: es,
                    }) => {
                        // env path already registered for GC protection above (before spawn)

                        // Store launched config for env sync drift detection
                        {
                            let mut lc = room.runtime_agent_launched_config.write().await;
                            *lc = Some(launched_config.clone());
                        }

                        let es_label = es.as_str().to_string();
                        publish_kernel_state_presence(
                            room,
                            presence::KernelStatus::Idle,
                            &es_label,
                        )
                        .await;

                        // Write Running(Idle) + kernel info to RuntimeStateDoc
                        // so frontends see "idle" via CRDT sync.
                        if let Err(e) = room.state.with_doc(|sd| {
                            sd.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Idle))?;
                            sd.set_kernel_info(kernel_type, kernel_type, &es_label)?;
                            sd.set_runtime_agent_id(&runtime_agent_id)?;
                            // Fresh kernel is in sync with its launched config
                            sd.set_env_sync(true, &[], &[], false, false)?;
                            Ok(())
                        }) {
                            warn!("[runtime-state] {}", e);
                        }

                        info!(
                            "[notebook-sync] Auto-launch via runtime agent succeeded: {} kernel with {} environment",
                            kernel_type, es_label
                        );
                    }
                    Ok(notebook_protocol::protocol::RuntimeAgentResponse::Error { error }) => {
                        warn!("[notebook-sync] Agent kernel launch failed: {}", error);
                        // Surface the failure through CRDT so the UI
                        // doesn't sit on "starting" forever. The error
                        // string usually carries the stderr tail from
                        // `jupyter_kernel.rs` — exactly what the user
                        // needs to see.
                        reset_starting_state_with_outcome(
                            room,
                            Some(&runtime_agent_id),
                            ResetOutcome::Error {
                                reason: None,
                                details: &error,
                            },
                        )
                        .await;
                    }
                    Ok(_) => {
                        warn!(
                            "[notebook-sync] Unexpected runtime agent response during auto-launch"
                        );
                        reset_starting_state_with_outcome(
                            room,
                            Some(&runtime_agent_id),
                            ResetOutcome::Error {
                                reason: None,
                                details: "Unexpected runtime agent response during kernel launch",
                            },
                        )
                        .await;
                    }
                    Err(e) => {
                        warn!("[notebook-sync] Agent communication error: {}", e);
                        let details = format!("Agent communication error: {e}");
                        reset_starting_state_with_outcome(
                            room,
                            Some(&runtime_agent_id),
                            ResetOutcome::Error {
                                reason: None,
                                details: &details,
                            },
                        )
                        .await;
                    }
                }
            }
            Err(e) => {
                warn!("[notebook-sync] Failed to spawn runtime agent: {}", e);
                let details = format!("Failed to spawn runtime agent: {e}");
                reset_starting_state_with_outcome(
                    room,
                    None,
                    ResetOutcome::Error {
                        reason: None,
                        details: &details,
                    },
                )
                .await;
            }
        }
    }
}

/// Publish the daemon's `KernelState` presence so late-joining peers
/// receive kernel status in their `PresenceSnapshot`.
pub(crate) async fn publish_kernel_state_presence(
    room: &NotebookRoom,
    status: presence::KernelStatus,
    env_source: &str,
) {
    update_kernel_presence(
        &room.broadcasts.presence,
        &room.broadcasts.presence_tx,
        status,
        env_source,
    )
    .await;
}

/// Update kernel state in the shared presence state and relay to all peers.
///
/// Factored out so spawned tasks (which only hold cloned Arcs) can call it
/// without needing a full `&NotebookRoom` reference.
pub(crate) async fn update_kernel_presence(
    presence_state: &Arc<RwLock<PresenceState>>,
    presence_tx: &broadcast::Sender<(String, Vec<u8>)>,
    status: presence::KernelStatus,
    env_source: &str,
) {
    let data = presence::KernelStateData {
        status,
        env_source: env_source.to_string(),
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    presence_state.write().await.update_peer(
        "daemon",
        "daemon",
        None,
        presence::ChannelData::KernelState(data.clone()),
        now_ms,
    );
    match presence::encode_kernel_state_update("daemon", &data) {
        Ok(bytes) => {
            let _ = presence_tx.send(("daemon".to_string(), bytes));
        }
        Err(e) => warn!(
            "[notebook-sync] Failed to encode kernel state presence: {}",
            e
        ),
    }
}

/// Promote inline deps from CRDT metadata to a project file.
///
/// When a kernel is project-backed (pixi:toml, uv:pyproject, or
/// conda:env_yml), inline deps in the CRDT are notebook intent. This function
/// promotes new inline deps to the project file without mirroring the full
/// project-file dependency set back into notebook metadata.
///
/// Find the byte offset in an environment.yml string where new dependencies
/// should be inserted (end of the `dependencies:` list, before the next
/// top-level key or EOF).
/// Where and how to insert new conda deps into an environment.yml.
pub(crate) struct EnvYmlInsertionPoint {
    /// Byte offset where new lines should be inserted.
    pub offset: usize,
    /// The indent prefix to use for each new `- dep` line (e.g. `"  "`).
    pub indent: String,
    /// The line ending used by the file (`"\n"` or `"\r\n"`).
    pub newline: &'static str,
    /// True when the content before `offset` doesn't end with a newline
    /// (e.g. file has no trailing newline). Callers must prepend `newline`
    /// before the first inserted dep.
    pub needs_leading_newline: bool,
}

/// Find the insertion point for new conda deps in an environment.yml string.
///
/// Returns the byte offset, detected indent, and line ending style so callers
/// can insert deps that match the file's existing formatting. Uses
/// `split_inclusive` for correct byte offsets on both LF and CRLF files.
pub(crate) fn find_env_yml_deps_insertion_point(content: &str) -> Option<EnvYmlInsertionPoint> {
    let newline: &'static str = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };

    let mut in_deps = false;
    let mut last_dep_end: Option<usize> = None;
    let mut baseline_indent: Option<String> = None;
    let mut byte_offset = 0usize;

    for raw_line in content.split_inclusive('\n') {
        let line_len = raw_line.len();
        let line = raw_line.trim_end_matches('\n').trim_end_matches('\r');
        let trimmed = line.trim();

        if !line.starts_with(' ') && !line.starts_with('\t') {
            if trimmed == "dependencies:" {
                in_deps = true;
                last_dep_end = Some(byte_offset + line_len);
                byte_offset += line_len;
                continue;
            } else if in_deps && !trimmed.is_empty() && !trimmed.starts_with('#') {
                break;
            }
        }

        if in_deps && trimmed.starts_with("- ") {
            let indent_str = &line[..line.len() - line.trim_start().len()];
            match &baseline_indent {
                None => baseline_indent = Some(indent_str.to_string()),
                Some(base) if indent_str != base.as_str() => {
                    byte_offset += line_len;
                    continue;
                }
                _ => {}
            }
            // Skip YAML sub-section entries like "- pip:"
            let entry = trimmed.trim_start_matches("- ");
            if entry.ends_with(':') && !entry.contains(' ') {
                byte_offset += line_len;
                continue;
            }
            last_dep_end = Some(byte_offset + line_len);
        }

        byte_offset += line_len;
    }

    last_dep_end.map(|offset| {
        let needs_leading_newline = offset > 0 && content.as_bytes()[offset - 1] != b'\n';
        EnvYmlInsertionPoint {
            offset,
            indent: baseline_indent.unwrap_or_else(|| "  ".to_string()),
            newline,
            needs_leading_newline,
        }
    })
}

/// Parsed package names from an environment.yml, split by namespace.
pub(crate) struct EnvYmlPackageNames {
    pub conda: std::collections::HashSet<String>,
    pub pip: std::collections::HashSet<String>,
}

/// Extract package names from an environment.yml string, split into conda and
/// pip namespaces. Names are lowercased for case-insensitive comparison.
pub(crate) fn extract_env_yml_package_names(content: &str) -> EnvYmlPackageNames {
    use rattler_conda_types::EnvironmentYaml;
    let mut result = EnvYmlPackageNames {
        conda: std::collections::HashSet::new(),
        pip: std::collections::HashSet::new(),
    };

    let Ok(env) = EnvironmentYaml::from_yaml_str(content) else {
        warn!(
            "[notebook-sync] Failed to parse environment.yml for dedup; skipping duplicate check"
        );
        return result;
    };

    for spec in env.match_specs() {
        if let rattler_conda_types::PackageNameMatcher::Exact(name) = &spec.name {
            let n = name.as_normalized().to_string().to_lowercase();
            if !n.is_empty() {
                result.conda.insert(n);
            }
        }
    }

    if let Some(pip_specs) = env.pip_specs() {
        for spec in pip_specs {
            let name = notebook_doc::metadata::extract_package_name(spec);
            if !name.is_empty() {
                result.pip.insert(name);
            }
        }
    }

    result
}

/// For removals, compares CRDT deps against launched baseline and runs
/// `pixi remove` / `uv remove` for any deps that were removed.
///
/// Returns the list of packages that were added or removed.
pub(crate) async fn promote_inline_deps_to_project(
    room: &NotebookRoom,
    env_source: &str,
    launched_config: &notebook_protocol::protocol::LaunchedEnvConfig,
) -> Result<Vec<String>, String> {
    use notebook_protocol::connection::EnvSource;
    let parsed_env_source = EnvSource::parse(env_source);
    let current_metadata = {
        let doc = room.doc.read().await;
        doc.get_metadata_snapshot()
    };
    let Some(current_metadata) = current_metadata else {
        return Ok(vec![]);
    };

    let mut promoted = Vec::new();

    if matches!(parsed_env_source, EnvSource::PixiToml) {
        let pixi_toml_path = launched_config.pixi_toml_path.as_ref().ok_or_else(|| {
            "pixi:toml kernel has no pixi_toml_path in launched config".to_string()
        })?;

        // Determine what deps are in the CRDT vs what was launched
        let current_deps: Vec<String> = current_metadata
            .runt
            .pixi
            .as_ref()
            .map(|p| p.dependencies.clone())
            .unwrap_or_default();
        let launched_deps: Vec<String> = launched_config.pixi_toml_deps.clone().unwrap_or_default();

        // Find additions: deps in CRDT but not in launched baseline.
        // Launched baseline uses "name = version" format; CRDT uses package names.
        // Compare by extracted package name.
        let launched_names: std::collections::HashSet<String> = launched_deps
            .iter()
            .map(|d| d.split('=').next().unwrap_or(d).trim().to_lowercase())
            .collect();

        let to_add: Vec<&str> = current_deps
            .iter()
            .filter(|d| {
                let name = notebook_doc::metadata::extract_package_name(d).to_lowercase();
                !launched_names.contains(&name)
            })
            .map(|d| d.as_str())
            .collect();

        // Note: we only ADD deps to the project file, never remove. The CRDT
        // only tracks deps added through the notebook — it doesn't represent
        // the full project dep set. Removing deps that are "in project but not
        // in CRDT" would destroy project deps the notebook doesn't know about.

        let pixi_path = kernel_launch::tools::get_pixi_path()
            .await
            .map_err(|e| format!("Failed to find pixi: {e}"))?;

        let mut errors = Vec::new();

        for dep in &to_add {
            info!("[notebook-sync] Promoting dep to pixi.toml: {}", dep);
            match tokio::process::Command::new(&pixi_path)
                .args(["add", dep, "--manifest-path"])
                .arg(pixi_toml_path)
                .output()
                .await
            {
                Ok(output) if output.status.success() => {
                    promoted.push(format!("+{}", dep));
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    errors.push(format!("pixi add {} failed: {}", dep, stderr.trim()));
                }
                Err(e) => {
                    errors.push(format!("Failed to run pixi add {}: {}", dep, e));
                }
            }
        }

        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
    } else if matches!(parsed_env_source, EnvSource::EnvYml) {
        let yml_path = launched_config
            .environment_yml_path
            .as_ref()
            .ok_or_else(|| {
                "conda:env_yml kernel has no environment_yml_path in launched config".to_string()
            })?;

        let current_deps: Vec<String> = current_metadata
            .runt
            .conda
            .as_ref()
            .map(|c| c.dependencies.clone())
            .unwrap_or_default();
        // Launched baseline = environment.yml deps snapshot at launch time
        let launched_deps = launched_config
            .environment_yml_deps
            .clone()
            .unwrap_or_default();

        let launched_names: std::collections::HashSet<String> = launched_deps
            .iter()
            .map(|d| notebook_doc::metadata::extract_package_name(d).to_lowercase())
            .collect();

        let to_add: Vec<&str> = current_deps
            .iter()
            .filter(|d| {
                let name = notebook_doc::metadata::extract_package_name(d).to_lowercase();
                !launched_names.contains(&name)
            })
            .map(|d| d.as_str())
            .collect();

        // For conda:env_yml, we append new deps to environment.yml directly.
        // This is a simple text-based approach — append to the dependencies: section.
        let mut errors = Vec::new();

        if !to_add.is_empty() {
            match std::fs::read_to_string(yml_path) {
                Ok(content) => {
                    // Dedup against conda names already in the file — not pip,
                    // since a user may intentionally promote a conda dep that
                    // also exists under pip:.
                    let existing_names = extract_env_yml_package_names(&content).conda;
                    let to_add: Vec<&str> = to_add
                        .into_iter()
                        .filter(|d| {
                            let name =
                                notebook_doc::metadata::extract_package_name(d).to_lowercase();
                            !existing_names.contains(&name)
                        })
                        .collect();

                    if !to_add.is_empty() {
                        let mut new_content = content.clone();
                        let insertion = find_env_yml_deps_insertion_point(&content);
                        if let Some(ins) = insertion {
                            let mut insert_str = String::new();
                            if ins.needs_leading_newline {
                                insert_str.push_str(ins.newline);
                            }
                            for dep in &to_add {
                                insert_str
                                    .push_str(&format!("{}- {}{}", ins.indent, dep, ins.newline));
                                promoted.push(format!("+{}", dep));
                            }
                            new_content.insert_str(ins.offset, &insert_str);
                        } else {
                            new_content.push_str("\ndependencies:\n");
                            for dep in &to_add {
                                new_content.push_str(&format!("  - {}\n", dep));
                                promoted.push(format!("+{}", dep));
                            }
                        }
                        // Validate the result parses before writing
                        if let Err(e) =
                            rattler_conda_types::EnvironmentYaml::from_yaml_str(&new_content)
                        {
                            errors.push(format!("Inserted deps produced invalid YAML: {}", e));
                        } else if let Err(e) = std::fs::write(yml_path, &new_content) {
                            errors.push(format!("Failed to write environment.yml: {}", e));
                        }
                    }
                }
                Err(e) => {
                    errors.push(format!("Failed to read environment.yml: {}", e));
                }
            }
        }

        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
    } else if matches!(parsed_env_source, EnvSource::Pyproject) {
        let pyproject_path = launched_config.pyproject_path.as_ref().ok_or_else(|| {
            "uv:pyproject kernel has no pyproject_path in launched config".to_string()
        })?;
        let project_dir = pyproject_path
            .parent()
            .ok_or_else(|| "Cannot determine project directory".to_string())?;

        let current_deps: Vec<String> = current_metadata
            .runt
            .uv
            .as_ref()
            .map(|u| u.dependencies.clone())
            .unwrap_or_default();
        // For uv:pyproject, the launched baseline is the pyproject.toml deps.
        // Read them from the file for comparison.
        let launched_deps = if let Ok(content) = std::fs::read_to_string(pyproject_path) {
            extract_pyproject_deps(&content)
        } else {
            vec![]
        };

        let launched_names: std::collections::HashSet<String> = launched_deps
            .iter()
            .map(|d| notebook_doc::metadata::extract_package_name(d).to_lowercase())
            .collect();

        let to_add: Vec<&str> = current_deps
            .iter()
            .filter(|d| {
                let name = notebook_doc::metadata::extract_package_name(d).to_lowercase();
                !launched_names.contains(&name)
            })
            .map(|d| d.as_str())
            .collect();

        // Note: we only ADD deps to the project file, never remove. The CRDT
        // only tracks deps added through the notebook — it doesn't represent
        // the full project dep set. Removing deps that are "in project but not
        // in CRDT" would destroy project deps the notebook doesn't know about.

        let uv_path = kernel_launch::tools::get_uv_path()
            .await
            .map_err(|e| format!("Failed to find uv: {e}"))?;

        let mut errors = Vec::new();

        for dep in &to_add {
            info!("[notebook-sync] Promoting dep to pyproject.toml: {}", dep);
            match tokio::process::Command::new(&uv_path)
                .args(["add", dep, "--project"])
                .arg(project_dir)
                .output()
                .await
            {
                Ok(output) if output.status.success() => {
                    promoted.push(format!("+{}", dep));
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    errors.push(format!("uv add {} failed: {}", dep, stderr.trim()));
                }
                Err(e) => {
                    errors.push(format!("Failed to run uv add {}: {}", dep, e));
                }
            }
        }

        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
    }

    Ok(promoted)
}

/// Handle sync environment request - hot-install new packages without kernel restart.
///
/// Only supported for UV inline dependencies when there are only additions (no removals).
/// Conda and other env types fall back to restart.
pub(crate) async fn handle_sync_environment(room: &NotebookRoom) -> NotebookResponse {
    // Read launched config from room
    let launched = {
        let guard = room.runtime_agent_launched_config.read().await;
        match &*guard {
            Some(lc) => lc.clone(),
            None => {
                return NotebookResponse::SyncEnvironmentFailed {
                    error: "No kernel running".to_string(),
                    needs_restart: false,
                };
            }
        }
    };

    // For project-backed environments, promote inline deps to the project file.
    // Project envs can't hot-install — they need a kernel restart.
    let env_source = room
        .state
        .read(|sd| sd.read_state().kernel.env_source.clone())
        .unwrap_or_default();
    if env_source == "pixi:toml" || env_source == "uv:pyproject" || env_source == "conda:env_yml" {
        match promote_inline_deps_to_project(room, &env_source, &launched).await {
            Ok(promoted) if promoted.is_empty() => {
                return NotebookResponse::SyncEnvironmentComplete {
                    synced_packages: vec![],
                };
            }
            Ok(promoted) => {
                return NotebookResponse::SyncEnvironmentFailed {
                    error: format!(
                        "Updated project file ({}). Restart kernel to apply changes.",
                        promoted.join(", ")
                    ),
                    needs_restart: true,
                };
            }
            Err(e) => {
                return NotebookResponse::SyncEnvironmentFailed {
                    error: e,
                    needs_restart: false,
                };
            }
        }
    }

    // Read current metadata from the doc
    let current_metadata = {
        let doc = room.doc.read().await;
        doc.get_metadata_snapshot()
    };

    let Some(current_metadata) = current_metadata else {
        return NotebookResponse::SyncEnvironmentComplete {
            synced_packages: vec![],
        };
    };

    // Determine what packages need installing.
    // For inline-dep kernels, compute_env_sync_diff gives the drift.
    // For prewarmed kernels, check if the user added new inline deps.
    let is_tracking = launched.uv_deps.is_some()
        || launched.conda_deps.is_some()
        || launched.deno_config.is_some();

    let (packages_to_install, env_kind) = if is_tracking {
        // Kernel launched with inline deps — compute drift
        let diff = compute_env_sync_diff(&launched, &current_metadata);
        let Some(diff) = diff else {
            return NotebookResponse::SyncEnvironmentComplete {
                synced_packages: vec![],
            };
        };
        if !diff.removed.is_empty() || diff.channels_changed || diff.deno_changed {
            return NotebookResponse::SyncEnvironmentFailed {
                error: "Environment changes include removals or config changes that require a kernel restart".to_string(),
                needs_restart: true,
            };
        }
        let env_kind = if launched.uv_deps.is_some() {
            notebook_protocol::protocol::EnvKind::Uv {
                packages: diff.added.clone(),
            }
        } else if launched.conda_deps.is_some() {
            notebook_protocol::protocol::EnvKind::Conda {
                packages: diff.added.clone(),
                channels: get_inline_conda_channels(&current_metadata),
            }
        } else {
            return NotebookResponse::SyncEnvironmentFailed {
                error: "Hot-sync only supported for UV and Conda environments".to_string(),
                needs_restart: true,
            };
        };
        (diff.added, env_kind)
    } else {
        // Prewarmed kernel — check if user added inline deps
        let inline = check_inline_deps(&current_metadata);
        use notebook_protocol::connection::{EnvSource, PackageManager};
        match &inline {
            Some(EnvSource::Inline(PackageManager::Uv)) => {
                let added = get_inline_uv_deps(&current_metadata).unwrap_or_default();
                if added.is_empty() {
                    return NotebookResponse::SyncEnvironmentComplete {
                        synced_packages: vec![],
                    };
                }
                let env_kind = notebook_protocol::protocol::EnvKind::Uv {
                    packages: added.clone(),
                };
                (added, env_kind)
            }
            Some(EnvSource::Inline(PackageManager::Conda)) => {
                let added = get_inline_conda_deps(&current_metadata).unwrap_or_default();
                if added.is_empty() {
                    return NotebookResponse::SyncEnvironmentComplete {
                        synced_packages: vec![],
                    };
                }
                let env_kind = notebook_protocol::protocol::EnvKind::Conda {
                    packages: added.clone(),
                    channels: get_inline_conda_channels(&current_metadata),
                };
                (added, env_kind)
            }
            _ => {
                return NotebookResponse::SyncEnvironmentFailed {
                    error: "Hot-sync only supported for UV and Conda environments".to_string(),
                    needs_restart: true,
                };
            }
        }
    };

    if packages_to_install.is_empty() {
        return NotebookResponse::SyncEnvironmentComplete {
            synced_packages: vec![],
        };
    }

    // Send SyncEnvironment to the runtime agent
    let sync_request =
        notebook_protocol::protocol::RuntimeAgentRequest::SyncEnvironment(env_kind.clone());

    match send_runtime_agent_request(room, sync_request).await {
        Ok(notebook_protocol::protocol::RuntimeAgentResponse::EnvironmentSynced {
            synced_packages,
        }) => {
            // Update runtime_agent_launched_config to include new packages
            {
                let mut lc = room.runtime_agent_launched_config.write().await;
                if let Some(ref mut config) = *lc {
                    match &env_kind {
                        notebook_protocol::protocol::EnvKind::Uv { .. } => {
                            // Promote prewarmed to uv:inline baseline if needed
                            if config.uv_deps.is_none() {
                                config.uv_deps = Some(vec![]);
                            }
                            if let Some(ref mut deps) = config.uv_deps {
                                for pkg in &synced_packages {
                                    if !deps.contains(pkg) {
                                        deps.push(pkg.clone());
                                    }
                                }
                            }
                        }
                        notebook_protocol::protocol::EnvKind::Conda { .. } => {
                            // Promote prewarmed to conda:inline baseline if needed
                            if config.conda_deps.is_none() {
                                config.conda_deps = Some(vec![]);
                            }
                            if let Some(ref mut deps) = config.conda_deps {
                                for pkg in &synced_packages {
                                    if !deps.contains(pkg) {
                                        deps.push(pkg.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Mark as in sync in RuntimeStateDoc
            if let Err(e) = room
                .state
                .with_doc(|sd| sd.set_env_sync(true, &[], &[], false, false))
            {
                warn!("[runtime-state] {}", e);
            }

            NotebookResponse::SyncEnvironmentComplete { synced_packages }
        }
        Ok(notebook_protocol::protocol::RuntimeAgentResponse::Error { error }) => {
            error!("[notebook-sync] SyncEnvironment failed: {}", error);
            NotebookResponse::SyncEnvironmentFailed {
                error,
                needs_restart: true,
            }
        }
        Ok(_) => {
            error!("[notebook-sync] SyncEnvironment received unexpected response type");
            NotebookResponse::SyncEnvironmentFailed {
                error: "Unexpected runtime agent response".to_string(),
                needs_restart: true,
            }
        }
        Err(e) => {
            error!(
                "[notebook-sync] SyncEnvironment agent communication error: {}",
                e
            );
            NotebookResponse::SyncEnvironmentFailed {
                error: format!("Agent communication error: {}", e),
                needs_restart: false,
            }
        }
    }
}

/// Format a single source string using ruff (Python) or deno fmt (Deno).
///
/// Returns the formatted source with trailing newline stripped (cells shouldn't
/// end with \n), or None if formatting failed or wasn't applicable.
pub(crate) async fn format_source(source: &str, runtime: &str) -> Option<String> {
    use kernel_launch::tools;
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;

    if source.trim().is_empty() {
        return None;
    }

    let raw = match runtime {
        "python" => {
            let ruff_path = tools::get_ruff_path().await.ok()?;
            let mut child = tokio::process::Command::new(&ruff_path)
                .args(["format", "--stdin-filename", "cell.py", "-"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .ok()?;

            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(source.as_bytes()).await.ok()?;
            }

            let output = child.wait_with_output().await.ok()?;
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        }
        "deno" => {
            let deno_path = tools::get_deno_path().await.ok()?;
            let mut child = tokio::process::Command::new(&deno_path)
                .args(["fmt", "--ext=ts", "-"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .ok()?;

            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(source.as_bytes()).await.ok()?;
            }

            let output = child.wait_with_output().await.ok()?;
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        }
        _ => None,
    }?;

    // Strip trailing newline (formatters always add one, but cells shouldn't have it)
    let formatted = raw.strip_suffix('\n').unwrap_or(&raw).to_string();

    if formatted != source {
        Some(formatted)
    } else {
        None
    }
}

/// Map a runtime name to its formatter's CRDT actor label.
pub(crate) fn formatter_actor(runtime: &str) -> String {
    let tool = match runtime {
        "python" => "ruff",
        other => other, // "deno" stays "deno"
    };
    format!("runtimed:{tool}")
}

/// Detect the runtime from room metadata, returning "python", "deno", or None.
pub(crate) async fn detect_room_runtime(room: &NotebookRoom) -> Option<String> {
    let doc = room.doc.read().await;
    doc.get_metadata_snapshot()
        .and_then(|snapshot| snapshot.detect_runtime())
}

/// Format all code cells in a notebook using ruff (Python) or deno fmt (Deno).
///
/// Reads the runtime type from the notebook metadata and formats accordingly.
/// Updates the Automerge doc with formatted sources and broadcasts changes.
/// Formatting errors are logged but don't fail the operation (best-effort).
pub(crate) async fn format_notebook_cells(room: &NotebookRoom) -> Result<usize, String> {
    let runtime = match detect_room_runtime(room).await {
        Some(rt) => rt,
        None => {
            info!("[format] Skipping format: unknown kernelspec (no formatter available)");
            return Ok(0);
        }
    };

    // Get all code cells
    let cells: Vec<(String, String)> = {
        let doc = room.doc.read().await;
        doc.get_cells()
            .into_iter()
            .filter(|cell| cell.cell_type == "code" && !cell.source.trim().is_empty())
            .map(|cell| (cell.id, cell.source))
            .collect()
    };

    if cells.is_empty() {
        return Ok(0);
    }

    // Fork BEFORE formatting so the baseline is the pre-format doc state.
    // Formatting ops on the fork are concurrent with any user edits on the
    // live doc, and Automerge's text CRDT merges them cleanly.
    let mut fork = {
        let mut doc = room.doc.write().await;
        doc.fork_with_actor(format!(
            "{}:{}",
            formatter_actor(&runtime),
            uuid::Uuid::new_v4()
        ))
    };

    let mut formatted_count = 0;
    for (cell_id, source) in cells {
        if let Some(fmt) = format_source(&source, &runtime).await {
            if fork.update_source(&cell_id, &fmt).is_ok() {
                formatted_count += 1;
            }
        }
    }

    if formatted_count > 0 {
        let mut doc = room.doc.write().await;
        if let Err(e) = catch_automerge_panic("save-format-merge", || doc.merge(&mut fork)) {
            warn!("{}", e);
            doc.rebuild_from_save();
        }
        let _ = room.broadcasts.changed_tx.send(());
        info!(
            "[format] Formatted {} code cells (runtime: {})",
            formatted_count, runtime
        );
    }

    Ok(formatted_count)
}
