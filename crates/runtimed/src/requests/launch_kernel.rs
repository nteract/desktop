//! `NotebookRequest::LaunchKernel` handler.
//!
//! Biggest single arm — 1300+ lines. Resolves kernel type + env source,
//! promotes inline deps, claims pool envs or prepares inline caches, spawns
//! the runtime agent, and dispatches LaunchKernel / RestartKernel over RPC.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tokio::sync::oneshot;
use tracing::{error, info, warn};

use notebook_doc::presence;
use runtime_doc::{KernelActivity, KernelErrorReason, RuntimeLifecycle};

use crate::daemon::Daemon;
use crate::notebook_sync_server::{
    acquire_prewarmed_env_with_capture, build_launched_config, captured_env_for_runtime,
    captured_env_source_override, check_and_broadcast_sync_state, check_inline_deps,
    extract_pixi_toml_deps, get_inline_conda_channels, get_inline_conda_deps, get_inline_uv_deps,
    get_inline_uv_prerelease, promote_inline_deps_to_project, publish_kernel_state_presence,
    reset_starting_state, resolve_metadata_snapshot, send_runtime_agent_request,
    try_conda_pool_for_inline_deps, try_uv_pool_for_inline_deps, unified_env_on_disk,
    CapturedEnvRuntime, NotebookRoom,
};
use crate::protocol::NotebookResponse;

pub(crate) async fn handle(
    room: &Arc<NotebookRoom>,
    daemon: &Arc<Daemon>,
    kernel_type: String,
    env_source: String,
    notebook_path: Option<String>,
) -> NotebookResponse {
    // Fall back to the room's on-disk path when the caller doesn't
    // supply one. The frontend typically launches with
    // `notebook_path: None` and relies on the room knowing its own
    // path; without this fallback, notebook-relative working dirs
    // and auto-detection of `pyproject.toml` / `environment.yml` /
    // `pixi.toml` silently stop working for saved notebooks.
    let notebook_path = match notebook_path {
        Some(p) => Some(p),
        None => room
            .identity
            .path
            .read()
            .await
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
    };
    // Check RuntimeStateDoc for launch serialization.
    // Uses write lock so we can atomically check + set "starting"
    // to prevent two concurrent LaunchKernel requests from both
    // proceeding past this gate.
    //
    // Scope the write guard so it drops before any async work
    // (deadlock prevention: no lock held across `.await`).
    let prior_lifecycle = room
        .state
        .with_doc(|sd| {
            let prior = sd.read_state().kernel.lifecycle;
            let already_progressing = matches!(
                prior,
                RuntimeLifecycle::Running(_)
                    | RuntimeLifecycle::Resolving
                    | RuntimeLifecycle::PreparingEnv
                    | RuntimeLifecycle::Launching
                    | RuntimeLifecycle::Connecting
            );
            if !already_progressing {
                // Atomically claim the launch by moving into Resolving
                // while we hold the sync mutex. Prevents a concurrent
                // LaunchKernel from also proceeding past this gate.
                sd.clear_comms().ok();
                sd.set_trust("trusted", false).ok();
                sd.set_lifecycle(&RuntimeLifecycle::Resolving).ok();
            }
            Ok(prior)
        })
        .unwrap_or_else(|e| {
            warn!("[runtime-state] {}", e);
            RuntimeLifecycle::NotStarted
        });
    match prior_lifecycle {
        RuntimeLifecycle::Running(_) => {
            // Agent already has a running kernel — check for restart path below.
        }
        RuntimeLifecycle::Resolving
        | RuntimeLifecycle::PreparingEnv
        | RuntimeLifecycle::Launching
        | RuntimeLifecycle::Connecting => {
            // Another launch in progress — wait for it to complete.
            let wait_result = tokio::time::timeout(std::time::Duration::from_secs(60), async {
                loop {
                    let lc = room
                        .state
                        .read(|sd| sd.read_state().kernel.lifecycle)
                        .unwrap_or(RuntimeLifecycle::NotStarted);
                    if matches!(
                        lc,
                        RuntimeLifecycle::Running(_)
                            | RuntimeLifecycle::Error
                            | RuntimeLifecycle::Shutdown
                            | RuntimeLifecycle::NotStarted
                    ) {
                        return lc;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            })
            .await;

            match wait_result {
                Ok(RuntimeLifecycle::Running(_)) => {
                    // Launch completed — fall through to restart check below.
                }
                Ok(_) | Err(_) => {
                    return NotebookResponse::Error {
                        error: "Kernel launch timed out or failed".to_string(),
                    };
                }
            }
        }
        _ => {
            // Not_started / Error / Shutdown / AwaitingTrust — already
            // claimed above by writing Resolving; fall through.
        }
    }

    let notebook_path = notebook_path.map(std::path::PathBuf::from);
    // Fall back to room.identity.working_dir for untitled notebooks (mirrors auto-launch path).
    // Enables project file detection (environment.yaml, pyproject.toml, pixi.toml)
    // when MCP callers send notebook_path: None for UUID-based notebooks.
    let notebook_path = match notebook_path {
        some @ Some(_) => some,
        None => {
            let wd = room.identity.working_dir.read().await;
            wd.clone().inspect(|p| {
                info!(
                    "[notebook-sync] LaunchKernel: using room working_dir for project file detection: {}",
                    p.display()
                );
            })
        }
    };

    // Resolve metadata snapshot from Automerge doc (preferred) or disk
    let mut metadata_snapshot = resolve_metadata_snapshot(room, notebook_path.as_deref()).await;

    // Auto-detect kernel type if "auto" or empty
    let resolved_kernel_type = if kernel_type == "auto" || kernel_type.is_empty() {
        metadata_snapshot
            .as_ref()
            .and_then(|s| s.detect_runtime())
            .unwrap_or_else(|| {
                info!("[notebook-sync] LaunchKernel: kernel type unknown, defaulting to python");
                "python".to_string()
            })
    } else {
        kernel_type.clone()
    };
    info!(
        "[notebook-sync] LaunchKernel: resolved kernel_type='{}' (from '{}')",
        resolved_kernel_type, kernel_type
    );

    // Classify the request-time input. Permissive: non-canonical values land in
    // `LaunchSpec::Concrete(EnvSource::Unknown(_))` and pass through unchanged.
    use notebook_protocol::connection::{EnvSource, LaunchSpec, PackageManager};
    let launch_spec = LaunchSpec::parse(&env_source);

    // Deno kernels don't use Python environments - always use "deno" regardless
    // of what env_source was requested. Log a warning if caller passed a Python env.
    let resolved_env_source = if resolved_kernel_type == "deno" {
        match &launch_spec {
            LaunchSpec::Auto | LaunchSpec::AutoScoped(_) => {
                info!("[notebook-sync] Deno kernel detected, using 'deno' env_source");
            }
            LaunchSpec::Concrete(EnvSource::Deno) => {
                info!("[notebook-sync] Deno kernel detected, using 'deno' env_source");
            }
            LaunchSpec::Concrete(other) => {
                warn!(
                    "[notebook-sync] Deno kernel requested with Python env_source '{}' - \
                     ignoring and using 'deno' instead",
                    other.as_str()
                );
            }
        }
        EnvSource::Deno.as_str().to_string()
    } else if matches!(launch_spec, LaunchSpec::Auto | LaunchSpec::AutoScoped(_)) {
        // Auto-detect Python environment, optionally scoped to a package manager family.
        // "auto:uv" constrains to UV sources, "auto:conda" to conda sources,
        // "auto:pixi" to pixi sources.
        let auto_scope: Option<&'static str> = match launch_spec.auto_scope() {
            Some(PackageManager::Uv) => Some("uv"),
            Some(PackageManager::Conda) => Some("conda"),
            Some(PackageManager::Pixi) => Some("pixi"),
            Some(PackageManager::Unknown(_)) | None => None,
        };

        // Priority 1: Detect project files near notebook path.
        // Project file wins because inline deps get promoted to the
        // project file at sync/launch time (project is source of truth).
        // A project file added after capture means the user wants the
        // project env, not the stale captured one.
        if let Some(detected) = notebook_path.as_ref().and_then(|path| match auto_scope {
            Some("uv") => crate::project_file::find_nearest_project_file(
                path,
                &[crate::project_file::ProjectFileKind::PyprojectToml],
            ),
            Some("conda") => crate::project_file::find_nearest_project_file(
                path,
                &[crate::project_file::ProjectFileKind::EnvironmentYml],
            ),
            Some("pixi") => crate::project_file::find_nearest_project_file(
                path,
                &[crate::project_file::ProjectFileKind::PixiToml],
            ),
            _ => crate::project_file::detect_project_file(path),
        }) {
            info!(
                "[notebook-sync] Auto-detected project file: {:?} -> {}",
                detected.path,
                detected.to_env_source().as_str()
            );
            detected.to_env_source().as_str().to_string()
        }
        // Priority 2: Captured prewarmed env wins over inline deps.
        // Captured deps look structurally identical to user-authored
        // inline deps, so without this override, reopening a captured
        // notebook would route through the inline-deps path and miss
        // the already-claimed env. Ordering is project file > captured
        // > inline > default so a pyproject.toml added post-capture
        // still wins.
        //
        // Respects `auto_scope`: `auto:uv` with a conda-captured
        // notebook (or vice versa) falls through. `auto:pixi` always
        // falls through — no pixi capture path yet.
        else if let Some(captured_src) = captured_env_source_override(metadata_snapshot.as_ref())
            .filter(|src| {
                use notebook_protocol::connection::{EnvSource, PackageManager};
                match auto_scope {
                    Some("uv") => matches!(src, EnvSource::Prewarmed(PackageManager::Uv)),
                    Some("conda") => matches!(src, EnvSource::Prewarmed(PackageManager::Conda)),
                    Some("pixi") => false,
                    _ => true,
                }
            })
        {
            info!(
                "[notebook-sync] LaunchKernel: captured env on disk -> {}",
                captured_src.as_str()
            );
            captured_src.as_str().to_string()
        }
        // Priority 3: Check inline deps in notebook metadata
        else if let Some(inline_source) =
            metadata_snapshot
                .as_ref()
                .and_then(|snap| -> Option<String> {
                    use notebook_protocol::connection::{EnvSource, PackageManager};
                    match auto_scope {
                        Some("uv") => snap
                            .runt
                            .uv
                            .as_ref()
                            .filter(|uv| !uv.dependencies.is_empty())
                            .map(|_| EnvSource::Inline(PackageManager::Uv).as_str().to_string()),
                        Some("conda") => snap
                            .runt
                            .conda
                            .as_ref()
                            .filter(|c| !c.dependencies.is_empty())
                            .map(|_| {
                                EnvSource::Inline(PackageManager::Conda)
                                    .as_str()
                                    .to_string()
                            }),
                        Some("pixi") => snap
                            .runt
                            .pixi
                            .as_ref()
                            .filter(|p| !p.dependencies.is_empty())
                            .map(|_| EnvSource::Inline(PackageManager::Pixi).as_str().to_string()),
                        _ => check_inline_deps(snap)
                            .filter(|s| !matches!(s, EnvSource::Deno))
                            .map(|s| s.as_str().to_string()),
                    }
                })
        {
            info!(
                "[notebook-sync] Found inline deps in notebook metadata -> {}",
                inline_source
            );
            inline_source
        } else {
            // Priority 3: Check PEP 723 script blocks in cell source
            let has_pep723_deps = if auto_scope == Some("conda") {
                false
            } else {
                let cells = room.doc.read().await.get_cells();
                match notebook_doc::pep723::find_pep723_in_cells(&cells) {
                    Ok(Some(ref m)) if !m.dependencies.is_empty() => true,
                    Ok(_) => false,
                    Err(e) => {
                        warn!(
                            "[notebook-sync] Failed to parse PEP 723 script blocks: {}",
                            e
                        );
                        false
                    }
                }
            };

            if has_pep723_deps {
                let pep723_source = match auto_scope {
                    Some("uv") => "uv:pep723",
                    Some("pixi") => "pixi:pep723",
                    Some("conda") => unreachable!("conda scope skips PEP 723"),
                    _ => {
                        let default_env = daemon.default_python_env().await;
                        match default_env {
                            crate::settings_doc::PythonEnvType::Pixi => "pixi:pep723",
                            _ => "uv:pep723",
                        }
                    }
                };
                info!(
                    "[notebook-sync] Found PEP 723 deps in cell source ({})",
                    pep723_source
                );
                pep723_source.to_string()
            }
            // Priority 4: Fall back to prewarmed (scoped to family)
            else {
                let fallback = match auto_scope {
                    Some("conda") => "conda:prewarmed",
                    Some("pixi") => "pixi:prewarmed",
                    _ => {
                        let default_env = daemon.default_python_env().await;
                        match default_env {
                            crate::settings_doc::PythonEnvType::Conda => "conda:prewarmed",
                            crate::settings_doc::PythonEnvType::Pixi => "pixi:prewarmed",
                            _ => "uv:prewarmed",
                        }
                    }
                };
                info!(
                    "[notebook-sync] No project file detected, using {}",
                    fallback
                );
                fallback.to_string()
            }
        }
    } else {
        // Use explicit env_source (e.g., "uv:inline", "conda:inline")
        env_source.clone()
    };

    let parsed_resolved = EnvSource::parse(&resolved_env_source);

    // For pixi:toml, verify ipykernel is declared before launching.
    // Unlike uv (`uv run --with ipykernel`) and conda:env_yml (daemon
    // injects ipykernel into deps pre-sync), pixi does not auto-inject.
    //
    // Publish the typed `KernelErrorReason::MissingIpykernel` AFTER
    // `reset_starting_state` so the Error lifecycle survives. The prior
    // code only returned an `Error` response — the toolbar could
    // classify the spawn error generically but never rendered the
    // targeted remediation on the RPC path.
    if matches!(parsed_resolved, EnvSource::PixiToml) {
        let pixi_path = notebook_path.as_ref().and_then(|nb| {
            crate::project_file::detect_project_file(nb)
                .filter(|d| d.kind == crate::project_file::ProjectFileKind::PixiToml)
                .map(|d| d.path)
        });
        if let Some(ref path) = pixi_path {
            let has_ipykernel = match kernel_launch::tools::pixi_info(path).await {
                Ok(info) => info.has_ipykernel(),
                Err(_) => crate::project_file::pixi_toml_has_ipykernel(path),
            };
            if !has_ipykernel {
                warn!(
                    "[notebook-sync] pixi.toml at {:?} does not declare ipykernel",
                    path
                );
                // Publish the typed reason atomically. Don't call
                // reset_starting_state here — no runtime agent has spawned
                // yet on this path, and the auto-launch version in
                // notebook_sync_server/metadata.rs deliberately skips it
                // for the same reason. reset_starting_state writes
                // NotStarted first and releases the doc lock before our
                // Error write lands, giving a concurrent retry a window to
                // claim Resolving that we'd then clobber back to Error.
                let env_source_label = parsed_resolved.as_str().to_string();
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
                return NotebookResponse::Error {
                    error: "ipykernel not found in pixi.toml — run `pixi add ipykernel` in your project directory".to_string(),
                };
            }
        }
    }

    // For project-backed envs, promote any inline deps to the project
    // file before launching. This handles the case where add_dependency
    // wrote to CRDT metadata and then triggered a restart.
    if matches!(
        parsed_resolved,
        EnvSource::PixiToml | EnvSource::Pyproject | EnvSource::EnvYml
    ) {
        if let Some(ref snap) = metadata_snapshot {
            let has_inline = match parsed_resolved {
                EnvSource::PixiToml => snap
                    .runt
                    .pixi
                    .as_ref()
                    .is_some_and(|p| !p.dependencies.is_empty()),
                EnvSource::Pyproject => snap
                    .runt
                    .uv
                    .as_ref()
                    .is_some_and(|u| !u.dependencies.is_empty()),
                EnvSource::EnvYml => snap
                    .runt
                    .conda
                    .as_ref()
                    .is_some_and(|c| !c.dependencies.is_empty()),
                _ => false,
            };
            if has_inline {
                // Build a minimal launched config with project paths for promotion
                let mut promo_config = notebook_protocol::protocol::LaunchedEnvConfig::default();
                if matches!(parsed_resolved, EnvSource::PixiToml) {
                    promo_config.pixi_toml_path = notebook_path.as_ref().and_then(|p| {
                        crate::project_file::detect_project_file(p)
                            .filter(|d| d.kind == crate::project_file::ProjectFileKind::PixiToml)
                            .map(|d| d.path)
                    });
                    // Launched baseline = current pixi.toml deps (before promotion)
                    if let Some(ref path) = promo_config.pixi_toml_path {
                        if let Ok(content) = std::fs::read_to_string(path) {
                            promo_config.pixi_toml_deps = Some(extract_pixi_toml_deps(&content));
                        }
                    }
                } else if matches!(parsed_resolved, EnvSource::EnvYml) {
                    promo_config.environment_yml_path = notebook_path.as_ref().and_then(|p| {
                        crate::project_file::find_nearest_project_file(
                            p,
                            &[crate::project_file::ProjectFileKind::EnvironmentYml],
                        )
                        .map(|d| d.path)
                    });
                    // Launched baseline = current env.yml deps (before promotion)
                    if let Some(ref path) = promo_config.environment_yml_path {
                        if let Ok(env_config) = crate::project_file::parse_environment_yml(path) {
                            let mut deps = env_config.dependencies;
                            deps.sort();
                            promo_config.environment_yml_deps = Some(deps);
                        }
                    }
                } else {
                    promo_config.pyproject_path = notebook_path.as_ref().and_then(|p| {
                        crate::project_file::detect_project_file(p)
                            .filter(|d| {
                                d.kind == crate::project_file::ProjectFileKind::PyprojectToml
                            })
                            .map(|d| d.path)
                    });
                }
                match promote_inline_deps_to_project(room, &resolved_env_source, &promo_config)
                    .await
                {
                    Ok(promoted) if !promoted.is_empty() => {
                        info!(
                            "[notebook-sync] Promoted deps to project file: {:?}",
                            promoted
                        );
                        // Re-read metadata snapshot after CRDT was updated
                        metadata_snapshot =
                            resolve_metadata_snapshot(room, notebook_path.as_deref()).await;
                    }
                    Err(e) => {
                        warn!("[notebook-sync] Failed to promote deps: {}", e);
                    }
                    _ => {}
                }
            }
        }
    }

    // Transition to "preparing_env" phase
    if let Err(e) = room
        .state
        .with_doc(|sd| sd.set_lifecycle(&RuntimeLifecycle::PreparingEnv))
    {
        warn!("[runtime-state] {}", e);
    }

    // Deno kernels don't need pooled environments
    let pooled_env = if resolved_kernel_type == "deno" {
        info!("[notebook-sync] LaunchKernel: Deno kernel (no pooled env)");
        None
    } else {
        // Python kernels require pooled environment
        match resolved_env_source.as_str() {
            "uv:prewarmed" | "conda:prewarmed" => {
                // Route through the capture-aware acquirer so:
                //  - Reopen path: if metadata has env_id + captured
                //    deps and the unified-hash env exists on disk,
                //    we cache-hit instead of taking from the pool.
                //  - First-launch path: take from pool, strip base,
                //    claim into `{cache}/{unified_hash}/`, write
                //    captured deps + env_id back into metadata.
                //
                // Without this, a manual LaunchKernel after capture
                // would take a fresh pool env instead of reusing
                // the claimed one, leaking envs and bypassing drift
                // detection's "captured baseline" logic.
                match acquire_prewarmed_env_with_capture(
                    &resolved_env_source,
                    daemon,
                    room,
                    metadata_snapshot.as_ref(),
                )
                .await
                {
                    Some(Some(env)) => {
                        info!(
                            "[notebook-sync] LaunchKernel: acquired {} env: {:?}",
                            resolved_env_source, env.python_path
                        );
                        Some(env)
                    }
                    Some(None) => None,
                    None => {
                        // `acquire_prewarmed_env_with_capture`
                        // already broadcast the error; bail out.
                        reset_starting_state(room, None).await;
                        return NotebookResponse::Error {
                            error: format!(
                                "{} pool empty - no environment available",
                                if matches!(
                                    parsed_resolved,
                                    EnvSource::Prewarmed(PackageManager::Uv)
                                ) {
                                    "UV"
                                } else {
                                    "Conda"
                                }
                            ),
                        };
                    }
                }
            }
            "uv:pyproject" | "uv:inline" | "uv:pep723" | "conda:inline" | "conda:env_yml"
            | "pixi:toml" | "pixi:inline" | "pixi:pep723" => {
                // These sources prepare their own environments, no pooled env needed
                info!(
                    "[notebook-sync] LaunchKernel: {} prepares its own env, no pool env",
                    resolved_env_source
                );
                None
            }
            other => {
                // For remaining conda sources, route to conda pool
                if other.starts_with("conda:") {
                    match daemon.take_conda_env().await {
                        Some(env) => Some(env),
                        None => {
                            reset_starting_state(room, None).await;
                            return NotebookResponse::Error {
                                error: "Conda pool empty".to_string(),
                            };
                        }
                    }
                } else {
                    // Prewarmed UV
                    match daemon.take_uv_env().await {
                        Some(env) => Some(env),
                        None => {
                            reset_starting_state(room, None).await;
                            return NotebookResponse::Error {
                                error: "UV pool empty".to_string(),
                            };
                        }
                    }
                }
            }
        }
    };

    // For inline deps, prepare a cached environment with rich progress
    let launch_progress_handler: std::sync::Arc<dyn kernel_env::ProgressHandler> =
        std::sync::Arc::new(crate::inline_env::BroadcastProgressHandler::new(
            room.broadcasts.kernel_broadcast_tx.clone(),
        ));

    // Fetch feature flags up front so inline env hashing matches
    // the kernel's install set (bootstrap_dx adds `dx`).
    let feature_flags_for_inline = daemon.feature_flags().await;
    let bootstrap_dx = feature_flags_for_inline.bootstrap_dx;

    let (pooled_env, inline_deps) = if matches!(
        parsed_resolved,
        EnvSource::Pep723(PackageManager::Uv)
    ) {
        // Extract PEP 723 deps from cell source
        let cells = room.doc.read().await.get_cells();
        let pep723_deps = match notebook_doc::pep723::find_pep723_in_cells(&cells) {
            Ok(Some(m)) if !m.dependencies.is_empty() => Some(m.dependencies),
            Ok(_) => None,
            Err(e) => {
                error!(
                    "[notebook-sync] Invalid PEP 723 metadata in notebook: {}",
                    e
                );
                reset_starting_state(room, None).await;
                return NotebookResponse::Error {
                    error: format!("Invalid PEP 723 metadata in notebook: {}", e),
                };
            }
        };

        if let Some(deps) = pep723_deps {
            info!(
                "[notebook-sync] LaunchKernel: Preparing cached UV env for PEP 723 deps: {:?}",
                deps
            );
            match crate::inline_env::prepare_uv_inline_env(
                &deps,
                None,
                launch_progress_handler.clone(),
                bootstrap_dx,
            )
            .await
            {
                Ok(prepared) => {
                    info!(
                        "[notebook-sync] LaunchKernel: Using cached PEP 723 env at {:?}",
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
                    error!("[notebook-sync] Failed to prepare PEP 723 env: {}", e);
                    reset_starting_state(room, None).await;
                    return NotebookResponse::Error {
                        error: format!("Failed to prepare PEP 723 environment: {}", e),
                    };
                }
            }
        } else {
            reset_starting_state(room, None).await;
            return NotebookResponse::Error {
                error: "No PEP 723 dependencies found in notebook cells for requested env_source \"uv:pep723\""
                    .to_string(),
            };
        }
    } else if matches!(parsed_resolved, EnvSource::Inline(PackageManager::Uv)) {
        if let Some(deps) = metadata_snapshot.as_ref().and_then(get_inline_uv_deps) {
            let prerelease = metadata_snapshot
                .as_ref()
                .and_then(get_inline_uv_prerelease);

            // Fast path: check inline env cache first (instant on hit)
            if let Some(cached) =
                crate::inline_env::check_uv_inline_cache(&deps, prerelease.as_deref(), bootstrap_dx)
            {
                info!(
                    "[notebook-sync] LaunchKernel: UV inline cache hit at {:?}",
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
                match try_uv_pool_for_inline_deps(&deps, daemon, launch_progress_handler.clone())
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
                            "[notebook-sync] LaunchKernel: Preparing cached UV env for inline deps: {:?}",
                            deps
                        );
                        match crate::inline_env::prepare_uv_inline_env(
                            &deps,
                            prerelease.as_deref(),
                            launch_progress_handler.clone(),
                            bootstrap_dx,
                        )
                        .await
                        {
                            Ok(prepared) => {
                                let env = Some(crate::PooledEnv {
                                    env_type: crate::EnvType::Uv,
                                    venv_path: prepared.env_path,
                                    python_path: prepared.python_path,
                                    prewarmed_packages: vec![],
                                });
                                (env, Some(deps))
                            }
                            Err(e) => {
                                reset_starting_state(room, None).await;
                                return NotebookResponse::Error {
                                    error: format!("Failed to prepare inline environment: {}", e),
                                };
                            }
                        }
                    }
                }
            } else {
                // Has prerelease — can't use pool, go straight to full build
                info!(
                    "[notebook-sync] LaunchKernel: Preparing cached UV env for inline deps: {:?} (prerelease: {:?})",
                    deps, prerelease
                );
                match crate::inline_env::prepare_uv_inline_env(
                    &deps,
                    prerelease.as_deref(),
                    launch_progress_handler.clone(),
                    bootstrap_dx,
                )
                .await
                {
                    Ok(prepared) => {
                        let env = Some(crate::PooledEnv {
                            env_type: crate::EnvType::Uv,
                            venv_path: prepared.env_path,
                            python_path: prepared.python_path,
                            prewarmed_packages: vec![],
                        });
                        (env, Some(deps))
                    }
                    Err(e) => {
                        reset_starting_state(room, None).await;
                        return NotebookResponse::Error {
                            error: format!("Failed to prepare inline environment: {}", e),
                        };
                    }
                }
            }
        } else {
            (pooled_env, None)
        }
    } else if matches!(parsed_resolved, EnvSource::Inline(PackageManager::Conda)) {
        if let Some(deps) = metadata_snapshot.as_ref().and_then(get_inline_conda_deps) {
            let channels = metadata_snapshot
                .as_ref()
                .map(get_inline_conda_channels)
                .unwrap_or_else(|| vec!["conda-forge".to_string()]);

            // Fast path: check inline env cache first (instant on hit)
            if let Some(cached) = crate::inline_env::check_conda_inline_cache(&deps, &channels) {
                info!(
                    "[notebook-sync] LaunchKernel: Conda inline cache hit at {:?}",
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
                    daemon,
                    launch_progress_handler.clone(),
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
                            "[notebook-sync] LaunchKernel: Preparing cached Conda env for inline deps: {:?} (channels: {:?})",
                            deps, channels
                        );
                        match crate::inline_env::prepare_conda_inline_env(
                            &deps,
                            &channels,
                            launch_progress_handler.clone(),
                        )
                        .await
                        {
                            Ok(prepared) => {
                                let env = Some(crate::PooledEnv {
                                    env_type: crate::EnvType::Conda,
                                    venv_path: prepared.env_path,
                                    python_path: prepared.python_path,
                                    prewarmed_packages: vec![],
                                });
                                (env, Some(deps))
                            }
                            Err(e) => {
                                reset_starting_state(room, None).await;
                                return NotebookResponse::Error {
                                    error: format!(
                                        "Failed to prepare conda inline environment: {}",
                                        e
                                    ),
                                };
                            }
                        }
                    }
                }
            }
        } else {
            (pooled_env, None)
        }
    } else if matches!(parsed_resolved, EnvSource::EnvYml) {
        // conda:env_yml: find or create a named conda env from environment.yml.
        // Uses standard conda env discovery: name: field → search conda env dirs,
        // prefix: field → use that path directly. Falls back to creating via rattler.
        let yml_path = notebook_path.as_ref().and_then(|p| {
            crate::project_file::find_nearest_project_file(
                p,
                &[crate::project_file::ProjectFileKind::EnvironmentYml],
            )
            .map(|d| d.path)
        });

        if let Some(ref yml) = yml_path {
            match crate::project_file::parse_environment_yml(yml) {
                Ok(env_config) => {
                    // Resolve the conda prefix: prefix: → direct path,
                    // name: → search standard dirs, create if not found.
                    let conda_prefix = if let Some(ref prefix) = env_config.prefix {
                        // Explicit prefix: path from environment.yml
                        prefix.clone()
                    } else if let Some(ref name) = env_config.name {
                        // Named env — search for existing, or determine creation path
                        crate::project_file::find_named_conda_env(name).unwrap_or_else(|| {
                            // Will create at default location
                            crate::project_file::default_conda_envs_dir().join(name)
                        })
                    } else {
                        // No name or prefix — use a hash-based env in cache
                        let cache_dir = crate::paths::default_cache_dir().join("conda-envs");
                        let conda_deps_tmp = kernel_env::CondaDependencies {
                            dependencies: env_config.dependencies.clone(),
                            channels: env_config.channels.clone(),
                            python: env_config.python.clone(),
                            env_id: None,
                        };
                        cache_dir.join(kernel_env::conda::compute_env_hash(&conda_deps_tmp))
                    };

                    // Merge env.yml deps with any CRDT notebook deps (additive)
                    let mut all_deps = env_config.dependencies.clone();
                    if let Some(crdt_deps) =
                        metadata_snapshot.as_ref().and_then(get_inline_conda_deps)
                    {
                        let base_names: std::collections::HashSet<String> = all_deps
                            .iter()
                            .map(|d| notebook_doc::metadata::extract_package_name(d).to_lowercase())
                            .collect();
                        for dep in &crdt_deps {
                            let name =
                                notebook_doc::metadata::extract_package_name(dep).to_lowercase();
                            if !base_names.contains(&name) {
                                all_deps.push(dep.clone());
                            }
                        }
                    }

                    // Always include ipykernel
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
                        // Existing env — sync deps into it
                        let conda_env = kernel_env::CondaEnvironment {
                            env_path: conda_prefix.clone(),
                            python_path: python_path.clone(),
                        };
                        launch_progress_handler.on_progress(
                            "conda",
                            kernel_env::EnvProgressPhase::Installing {
                                total: conda_deps.dependencies.len(),
                            },
                        );
                        if let Err(e) =
                            kernel_env::conda::sync_dependencies(&conda_env, &conda_deps).await
                        {
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
                        // No existing env — create it via rattler at the target path.
                        // prepare_environment_in creates {cache_dir}/{hash}/, so we
                        // pass the parent and then rename to the target name.
                        let parent = conda_prefix
                            .parent()
                            .unwrap_or_else(|| std::path::Path::new("/tmp"));
                        if let Err(e) = tokio::fs::create_dir_all(parent).await {
                            reset_starting_state(room, None).await;
                            return NotebookResponse::Error {
                                error: format!(
                                    "Failed to create conda envs directory {:?}: {}",
                                    parent, e
                                ),
                            };
                        }
                        match kernel_env::conda::prepare_environment_in(
                            &conda_deps,
                            parent,
                            launch_progress_handler.clone(),
                        )
                        .await
                        {
                            Ok(prepared) => {
                                // Rename hash-based dir to the target env name
                                let final_prefix = if prepared.env_path != conda_prefix {
                                    match tokio::fs::rename(&prepared.env_path, &conda_prefix).await
                                    {
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
                                reset_starting_state(room, None).await;
                                return NotebookResponse::Error {
                                    error: format!(
                                        "Failed to create conda env '{}' from environment.yml: {}",
                                        env_name_display, e
                                    ),
                                };
                            }
                        }
                    }
                }
                Err(e) => {
                    reset_starting_state(room, None).await;
                    return NotebookResponse::Error {
                        error: format!("Failed to parse environment.yml: {}", e),
                    };
                }
            }
        } else {
            warn!("[notebook-sync] conda:env_yml but no environment.yml found");
            (pooled_env, None)
        }
    } else if matches!(parsed_resolved, EnvSource::Inline(PackageManager::Pixi)) {
        // pixi exec handles its own caching — just extract deps for -w flags
        let deps = metadata_snapshot
            .as_ref()
            .and_then(|s| s.runt.pixi.as_ref())
            .map(|p| p.dependencies.clone())
            .unwrap_or_default();
        if !deps.is_empty() {
            info!(
                "[notebook-sync] LaunchKernel: pixi:inline deps for pixi exec: {:?}",
                deps
            );
            (None, Some(deps))
        } else {
            (pooled_env, None)
        }
    } else if matches!(parsed_resolved, EnvSource::Pep723(PackageManager::Pixi)) {
        // PEP 723 deps via pixi exec -w
        let cells = room.doc.read().await.get_cells();
        match notebook_doc::pep723::find_pep723_in_cells(&cells) {
            Ok(Some(meta)) if !meta.dependencies.is_empty() => {
                info!(
                    "[notebook-sync] LaunchKernel: pixi:pep723 deps: {:?}",
                    meta.dependencies
                );
                (None, Some(meta.dependencies))
            }
            _ => (pooled_env, None),
        }
    } else {
        (pooled_env, None)
    };

    // Verify ipykernel is present in the prepared env before we launch.
    // Mirrors the auto-launch gate in `notebook_sync_server/metadata.rs`:
    // the LaunchKernel RPC serves the toolbar restart button,
    // restart-and-run-all, post-trust approval, and other retry paths,
    // so a stale cache or hand-edited venv must surface the typed
    // `MissingIpykernel` reason here too — otherwise retries silently
    // regress to a generic kernel spawn failure. Skipped env sources
    // match the auto-launch site (prewarmed pools, pixi:*, uv:pyproject,
    // conda:env_yml, deno); pixi:toml is gated earlier in this function.
    if matches!(
        parsed_resolved,
        EnvSource::Inline(PackageManager::Uv)
            | EnvSource::Inline(PackageManager::Conda)
            | EnvSource::Pep723(PackageManager::Uv)
    ) {
        if let Some(ref env) = pooled_env {
            if !kernel_env::venv_has_ipykernel(&env.python_path) {
                warn!(
                    "[launch-kernel] prepared env at {:?} ({}) is missing ipykernel — cannot launch kernel",
                    env.venv_path,
                    parsed_resolved.as_str()
                );
                // Don't delete the env dir: for inline/pep723 it's a
                // content-addressed cache shared across notebooks and
                // vulnerable to a racy nuke of a concurrent install.
                //
                // Tear down the existing runtime agent (if any) BEFORE
                // we write the Error lifecycle. This path can be
                // reached from a restart (prior_lifecycle == Running),
                // in which case an agent is still holding request/
                // response channels. Leaving them up while the UI
                // flips to Error leaves the notebook split-brained
                // (old kernel still executes; UI thinks start failed).
                // `reset_starting_state` first writes NotStarted and
                // clears agent handles; we then overwrite lifecycle
                // with Error + MissingIpykernel so the UI sees the
                // correct reason. Minor TOCTOU against a concurrent
                // retry is accepted — a second retry observing Error
                // will simply start a fresh launch.
                reset_starting_state(room, None).await;
                let env_source_label = parsed_resolved.as_str().to_string();
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
                return NotebookResponse::Error {
                    error: format!(
                        "ipykernel not found in prepared {} environment",
                        parsed_resolved.as_str()
                    ),
                };
            }
        }
    }

    // Register the env path for GC protection immediately after pool.take(),
    // BEFORE any async work (agent spawn, connect timeout, delta install).
    if let Some(ref env) = pooled_env {
        let mut ep = room.runtime_agent_env_path.write().await;
        *ep = Some(env.venv_path.clone());
    }

    // Build LaunchedEnvConfig to track what config the kernel was launched with.
    //
    // For captured-prewarmed launches, pass the captured deps through
    // `captured_env_for_config` so `build_launched_config` records them
    // as the launch baseline. That way drift detection treats the
    // launch as "tracking" and won't falsely report captured deps as
    // pending additions on every reopen (see P3 in the codex review).
    //
    // `captured_env_for_config` must match the *final* resolved env
    // source — if the user explicitly asked for e.g. `uv:inline` and
    // we routed through inline flow, don't drag captured prewarmed
    // baselines along.
    let venv_path = pooled_env.as_ref().map(|e| e.venv_path.clone());
    let python_path = pooled_env.as_ref().map(|e| e.python_path.clone());
    let prewarmed_pkgs = pooled_env.as_ref().map(|e| e.prewarmed_packages.clone());
    let feature_flags = feature_flags_for_inline;
    let captured_env_for_config = match resolved_env_source.as_str() {
        "uv:prewarmed" => {
            captured_env_for_runtime(metadata_snapshot.as_ref(), CapturedEnvRuntime::Uv)
                .filter(|c| unified_env_on_disk(c).is_some())
        }
        "conda:prewarmed" => {
            captured_env_for_runtime(metadata_snapshot.as_ref(), CapturedEnvRuntime::Conda)
                .filter(|c| unified_env_on_disk(c).is_some())
        }
        _ => None,
    };
    let launched_config = build_launched_config(
        &resolved_kernel_type,
        &resolved_env_source,
        inline_deps.as_deref(),
        metadata_snapshot.as_ref(),
        venv_path,
        python_path,
        prewarmed_pkgs.as_deref(),
        notebook_path.as_deref(),
        feature_flags,
        captured_env_for_config.as_ref(),
    );

    // Transition to "launching" phase before starting the kernel process
    if let Err(e) = room
        .state
        .with_doc(|sd| sd.set_lifecycle(&RuntimeLifecycle::Launching))
    {
        warn!("[runtime-state] {}", e);
    }

    // If runtime agent is already connected, restart kernel in-place
    // (handles the shutdown → launch sequence without subprocess respawn)
    {
        let has_runtime_agent = room.runtime_agent_request_tx.lock().await.is_some();
        if has_runtime_agent {
            info!("[notebook-sync] Agent connected — sending RestartKernel");
            let restart_request = notebook_protocol::protocol::RuntimeAgentRequest::RestartKernel {
                kernel_type: resolved_kernel_type.clone(),
                env_source: resolved_env_source.clone(),
                notebook_path: notebook_path
                    .as_deref()
                    .map(|p| p.to_str().unwrap_or("").to_string()),
                launched_config: launched_config.clone(),
                env_vars: Default::default(),
            };
            match send_runtime_agent_request(room, restart_request).await {
                Ok(notebook_protocol::protocol::RuntimeAgentResponse::KernelRestarted {
                    env_source: es,
                }) => {
                    // Store launched config for env sync drift detection
                    {
                        let mut lc = room.runtime_agent_launched_config.write().await;
                        *lc = Some(launched_config.clone());
                    }

                    publish_kernel_state_presence(room, presence::KernelStatus::Idle, &es).await;
                    if let Err(e) = room.state.with_doc(|sd| {
                        sd.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Idle))?;
                        sd.set_kernel_info(&resolved_kernel_type, &resolved_kernel_type, &es)?;
                        sd.set_prewarmed_packages(&launched_config.prewarmed_packages)?;
                        // runtime_agent_id doesn't change on restart — same runtime agent
                        Ok(())
                    }) {
                        warn!("[runtime-state] {}", e);
                    }

                    // Compute env sync state against the freshly
                    // stored launched_config (updated above).
                    // Covers both inline-dep drift and the
                    // prewarmed-with-added-inline-deps case.
                    check_and_broadcast_sync_state(room).await;

                    return NotebookResponse::KernelLaunched {
                        kernel_type: resolved_kernel_type,
                        env_source: es,
                        launched_config,
                    };
                }
                Ok(notebook_protocol::protocol::RuntimeAgentResponse::Error { error }) => {
                    reset_starting_state(room, None).await;
                    return NotebookResponse::Error {
                        error: format!("Agent restart failed: {}", error),
                    };
                }
                Ok(_) => {
                    reset_starting_state(room, None).await;
                    return NotebookResponse::Error {
                        error: "Unexpected runtime agent response to RestartKernel".to_string(),
                    };
                }
                Err(e) => {
                    warn!(
                        "[notebook-sync] RestartKernel RPC failed: {} — spawning new runtime agent",
                        e
                    );
                    // Fall through to spawn new runtime agent below
                }
            }
        }
    }

    // Spawn runtime agent subprocess for kernel execution
    {
        info!("[notebook-sync] Spawning runtime agent subprocess");

        // Always pass the room UUID so the agent's RuntimeAgent
        // handshake finds the room in the UUID-keyed rooms map.
        let notebook_id = room.id.to_string();
        let runtime_agent_id = format!("runtime-agent:{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let socket_path = daemon.socket_path().clone();

        // Set provenance + bump generation + create oneshot BEFORE spawn
        // (see auto_launch_kernel for ordering rationale).
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
            notebook_id,
            runtime_agent_id.clone(),
            room.blob_store.root().to_path_buf(),
            socket_path,
        )
        .await
        {
            Ok(ra) => {
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

                // Wait for THIS runtime agent to connect back via socket
                match tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    runtime_agent_connect_rx,
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => {
                        reset_starting_state(room, Some(&runtime_agent_id)).await;
                        return NotebookResponse::Error {
                            error: "Runtime agent connect cancelled (superseded or died)"
                                .to_string(),
                        };
                    }
                    Err(_) => {
                        reset_starting_state(room, Some(&runtime_agent_id)).await;
                        return NotebookResponse::Error {
                            error: "Agent failed to connect within 30s".to_string(),
                        };
                    }
                }

                // Send LaunchKernel RPC
                let launch_request =
                    notebook_protocol::protocol::RuntimeAgentRequest::LaunchKernel {
                        kernel_type: resolved_kernel_type.clone(),
                        env_source: resolved_env_source.clone(),
                        notebook_path: notebook_path
                            .as_deref()
                            .map(|p| p.to_str().unwrap_or("").to_string()),
                        launched_config: launched_config.clone(),
                        env_vars: Default::default(),
                    };

                match send_runtime_agent_request(room, launch_request).await {
                    Ok(notebook_protocol::protocol::RuntimeAgentResponse::KernelLaunched {
                        env_source: es,
                    }) => {
                        // Store launched config for env sync drift detection
                        {
                            let mut lc = room.runtime_agent_launched_config.write().await;
                            *lc = Some(launched_config.clone());
                        }

                        publish_kernel_state_presence(room, presence::KernelStatus::Idle, &es)
                            .await;

                        // Write kernel status + info + prewarmed packages
                        // to RuntimeStateDoc
                        {
                            // Read agent ID before the sync mutex to
                            // avoid holding two locks.
                            let agent_id = room.current_runtime_agent_id.read().await.clone();
                            if let Err(e) = room.state.with_doc(|sd| {
                                sd.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Idle))?;
                                sd.set_kernel_info(
                                    &resolved_kernel_type,
                                    &resolved_kernel_type,
                                    &es,
                                )?;
                                sd.set_prewarmed_packages(&launched_config.prewarmed_packages)?;
                                if let Some(ref aid) = agent_id {
                                    sd.set_runtime_agent_id(aid)?;
                                }
                                Ok(())
                            }) {
                                warn!("[runtime-state] {}", e);
                            }
                        }

                        // Compute env sync state against the freshly
                        // stored launched_config (updated above).
                        check_and_broadcast_sync_state(room).await;

                        NotebookResponse::KernelLaunched {
                            kernel_type: resolved_kernel_type,
                            env_source: es,
                            launched_config,
                        }
                    }
                    Ok(notebook_protocol::protocol::RuntimeAgentResponse::Error { error }) => {
                        reset_starting_state(room, Some(&runtime_agent_id)).await;
                        NotebookResponse::Error {
                            error: format!("Agent kernel launch failed: {}", error),
                        }
                    }
                    Ok(_) => {
                        reset_starting_state(room, Some(&runtime_agent_id)).await;
                        NotebookResponse::Error {
                            error: "Unexpected runtime agent response".to_string(),
                        }
                    }
                    Err(e) => {
                        reset_starting_state(room, Some(&runtime_agent_id)).await;
                        NotebookResponse::Error {
                            error: format!("Agent communication error: {}", e),
                        }
                    }
                }
            }
            Err(e) => {
                reset_starting_state(room, None).await;
                NotebookResponse::Error {
                    error: format!("Failed to spawn runtime agent: {}", e),
                }
            }
        }
    }
}
