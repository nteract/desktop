use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct StreamOutputState {
    pub index: usize,
    pub blob_hash: String,
}

/// Project file the daemon picked for a notebook, identified by location
/// and kind. The parsed contents live under [`ProjectFileParsed`], carried
/// in the same [`ProjectContext::Detected`] state so every sync'd client
/// sees the same snapshot without an IPC round trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectFileKind {
    PyprojectToml,
    PixiToml,
    EnvironmentYml,
}

impl ProjectFileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PyprojectToml => "pyproject_toml",
            Self::PixiToml => "pixi_toml",
            Self::EnvironmentYml => "environment_yml",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pyproject_toml" => Some(Self::PyprojectToml),
            "pixi_toml" => Some(Self::PixiToml),
            "environment_yml" => Some(Self::EnvironmentYml),
            _ => None,
        }
    }
}

/// Pointer to a project file on the daemon's disk.
///
/// `absolute_path` is the file itself (not its parent). `relative_to_notebook`
/// is a display-friendly path that clients can render without re-walking
/// the filesystem - e.g. `"../pyproject.toml"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectFile {
    pub kind: ProjectFileKind,
    pub absolute_path: String,
    pub relative_to_notebook: String,
}

/// Parsed snapshot the daemon took when it wrote a [`ProjectContext::Detected`]
/// entry. Clients treat this as a time-stamped view: the file on disk can drift
/// when the user edits it externally, and there is no round-trip RPC to refresh
/// it synchronously. The daemon rewrites the snapshot on notebook open and on
/// whatever future triggers we add (file watch, save-as).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProjectFileParsed {
    /// Declared dependencies, in the form the source file used
    /// (e.g. `"pandas>=2.0"`, `"numpy"`, `"pip:requests"`).
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Dev-only dependencies. Currently populated from pyproject.toml's
    /// `[tool.uv.dev-dependencies]`; empty for pixi / environment.yml
    /// (they have their own sublist conventions in `extras`).
    #[serde(default)]
    pub dev_dependencies: Vec<String>,
    /// `requires-python` / Python constraint, when the file carries one.
    #[serde(default)]
    pub requires_python: Option<String>,
    /// uv's `--prerelease` strategy, when configured.
    #[serde(default)]
    pub prerelease: Option<String>,
    /// Kind-specific extras the three formats carry (conda channels,
    /// environment.yml pip sub-list, pixi PyPI deps, ...).
    #[serde(default)]
    pub extras: ProjectFileExtras,
}

/// Kind-specific parsed fields that do not fit the common shape.
///
/// Kept as a tagged enum so each variant only names fields it actually owns,
/// and adding a future kind (e.g. `conda-lock.yml`) is a closed-enum extension.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ProjectFileExtras {
    /// pyproject.toml has no kind-specific extras today.
    #[default]
    None,
    /// pixi-specific fields parsed out of `pixi.toml`.
    Pixi {
        #[serde(default)]
        channels: Vec<String>,
        /// Dependencies under `[pypi-dependencies]`, kept separate from
        /// conda-style entries under `dependencies`.
        #[serde(default)]
        pypi_dependencies: Vec<String>,
    },
    /// `environment.yml` carries conda channels and a pip sub-list
    /// that lives outside the main `dependencies` array.
    EnvironmentYml {
        #[serde(default)]
        channels: Vec<String>,
        #[serde(default)]
        pip: Vec<String>,
    },
}

/// Daemon-observed project context for a notebook. Written by the daemon
/// on notebook open and on any future refresh triggers we wire up. Clients
/// read; the frontend in particular reads this in place of walking the
/// filesystem itself.
///
/// Lifetime lines up with the enclosing [`RuntimeStateDoc`]: the field
/// survives kernel death. It goes stale when the notebook file moves on
/// disk, which the daemon does not currently re-walk for. The `observed_at`
/// timestamp lets clients honestly surface "as of T" rather than imply
/// live truth.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state")]
pub enum ProjectContext {
    /// Daemon has not walked yet. Initial state after notebook open and
    /// the state clients see before the first daemon write arrives via sync.
    #[default]
    Pending,

    /// Daemon walked, no project file in any parent directory.
    NotFound {
        /// When the daemon last confirmed "nothing here."
        observed_at: String,
    },

    /// Daemon walked and found a project file. `parsed` is the snapshot
    /// taken when this entry was written; `observed_at` is the truth
    /// timestamp.
    Detected {
        project_file: ProjectFile,
        parsed: ProjectFileParsed,
        observed_at: String,
    },

    /// Daemon walked, a file was there, but parsing failed. Distinct
    /// from `NotFound` so the UI can surface "your pyproject.toml is
    /// malformed" instead of silently showing nothing.
    Unreadable {
        path: String,
        reason: String,
        observed_at: String,
    },
}

impl ProjectContext {
    /// Variant name as a static string. Written to the CRDT `state` key
    /// and consumed by [`parse`](Self::parse) when reading.
    pub fn variant_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::NotFound { .. } => "not_found",
            Self::Detected { .. } => "detected",
            Self::Unreadable { .. } => "unreadable",
        }
    }
}

/// Observable activity of a running kernel.
///
/// Only meaningful when the runtime lifecycle is [`RuntimeLifecycle::Running`].
/// `Unknown` is the transient state between runtime agent connect and the
/// first IOPub status from the kernel; it also covers non-Jupyter backends
/// that do not report idle/busy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum KernelActivity {
    #[default]
    Unknown,
    Idle,
    Busy,
}

impl KernelActivity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Idle => "Idle",
            Self::Busy => "Busy",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "Unknown" => Some(Self::Unknown),
            "Idle" => Some(Self::Idle),
            "Busy" => Some(Self::Busy),
            _ => None,
        }
    }
}

/// Typed reason accompanying a [`RuntimeLifecycle::Error`] transition.
///
/// Closed enum by design. Every error reason the daemon surfaces gets
/// its own variant. This is deliberately more rigid than a free-form
/// string: reasons rarely change, and the compile-time guarantee that
/// the frontend and daemon agree on the vocabulary is worth the cost
/// of editing the enum.
///
/// [`as_str`](Self::as_str) returns the string written to
/// `kernel.error_reason` in the CRDT; the frontend mirrors the same
/// value via `KERNEL_ERROR_REASON` in `@runtimed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KernelErrorReason {
    /// Pixi-managed environment is missing the `ipykernel` package.
    /// `NotebookToolbar` gates its "install ipykernel" prompt on this.
    MissingIpykernel,
    /// environment.yml declares a conda env (by `name:` or `prefix:`) that
    /// isn't built on this machine. Daemon sets this instead of silently
    /// falling back to a pool env, so the frontend can tell the user what
    /// to do (`conda env create -f environment.yml`). Accompanying
    /// `error_details` carries the env name.
    CondaEnvYmlMissing,
}

impl KernelErrorReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingIpykernel => "missing_ipykernel",
            Self::CondaEnvYmlMissing => "conda_env_yml_missing",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "missing_ipykernel" => Some(Self::MissingIpykernel),
            "conda_env_yml_missing" => Some(Self::CondaEnvYmlMissing),
            _ => None,
        }
    }
}

/// Lifecycle of a runtime, from not-started through running to shutdown.
///
/// Typed sum replacing the earlier `(status, starting_phase)` string pair.
/// `Running` is the only variant that carries an activity, so it is
/// impossible to represent a "busy kernel that hasn't launched yet" in
/// the type system. Error details are carried out-of-band via
/// `KernelState::error_reason` so the enum stays `Eq`-able.
///
/// Serde format is tag+content:
/// - non-`Running` variants serialize as `{"lifecycle": "NotStarted"}`.
/// - `Running(activity)` serializes as `{"lifecycle": "Running", "activity": "Idle"}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "lifecycle", content = "activity")]
pub enum RuntimeLifecycle {
    #[default]
    NotStarted,
    AwaitingTrust,
    AwaitingEnvBuild,
    Resolving,
    PreparingEnv,
    Launching,
    Connecting,
    Running(KernelActivity),
    Error,
    Shutdown,
}

impl RuntimeLifecycle {
    /// Variant name as a static string (no payload).
    ///
    /// Written to `kernel/lifecycle` in the CRDT and consumed by
    /// [`to_legacy`](Self::to_legacy) for wire-protocol callers that
    /// still surface the compressed `(status, starting_phase)` pair.
    pub fn variant_str(&self) -> &'static str {
        match self {
            Self::NotStarted => "NotStarted",
            Self::AwaitingTrust => "AwaitingTrust",
            Self::AwaitingEnvBuild => "AwaitingEnvBuild",
            Self::Resolving => "Resolving",
            Self::PreparingEnv => "PreparingEnv",
            Self::Launching => "Launching",
            Self::Connecting => "Connecting",
            Self::Running(_) => "Running",
            Self::Error => "Error",
            Self::Shutdown => "Shutdown",
        }
    }

    /// Parse a `(lifecycle, activity)` pair.
    ///
    /// `activity` is consulted only when `lifecycle == "Running"`. An empty
    /// or unknown activity on a `Running` read is treated as
    /// [`KernelActivity::Unknown`] so consumers never observe a broken doc.
    pub fn parse(lifecycle: &str, activity: &str) -> Option<Self> {
        match lifecycle {
            "NotStarted" => Some(Self::NotStarted),
            "AwaitingTrust" => Some(Self::AwaitingTrust),
            "AwaitingEnvBuild" => Some(Self::AwaitingEnvBuild),
            "Resolving" => Some(Self::Resolving),
            "PreparingEnv" => Some(Self::PreparingEnv),
            "Launching" => Some(Self::Launching),
            "Connecting" => Some(Self::Connecting),
            "Running" => {
                let act = if activity.is_empty() {
                    KernelActivity::Unknown
                } else {
                    KernelActivity::parse(activity).unwrap_or(KernelActivity::Unknown)
                };
                Some(Self::Running(act))
            }
            "Error" => Some(Self::Error),
            "Shutdown" => Some(Self::Shutdown),
            _ => None,
        }
    }

    /// Derive a lifecycle from the pre-typed `(status, starting_phase)`
    /// string pair. Used as a fallback in [`resolve_lifecycle`] when
    /// reading a doc that predates the typed keys, so older producers
    /// still read correctly after callers upgrade.
    pub fn from_legacy(status: &str, starting_phase: &str) -> Self {
        match status {
            "idle" => Self::Running(KernelActivity::Idle),
            "busy" => Self::Running(KernelActivity::Busy),
            "starting" => match starting_phase {
                "resolving" => Self::Resolving,
                "preparing_env" => Self::PreparingEnv,
                "launching" => Self::Launching,
                "connecting" => Self::Connecting,
                // Unknown or empty sub-phase: fall back to the first
                // phase so consumers still see "we're starting" rather
                // than a default `NotStarted`.
                _ => Self::Resolving,
            },
            "error" => Self::Error,
            "shutdown" => Self::Shutdown,
            "awaiting_trust" => Self::AwaitingTrust,
            "awaiting_env_build" => Self::AwaitingEnvBuild,
            _ => Self::NotStarted,
        }
    }

    /// Project a lifecycle back to the `(status, starting_phase)` string
    /// pair for wire-protocol callers that still surface the compressed
    /// shape (`runt mcp`, `runt` CLI, `get_kernel_info` RPC).
    ///
    /// `Running(KernelActivity::Unknown)` projects to `("idle", "")`
    /// because the legacy shape had no "unknown" status. Callers that
    /// care about the distinction should match on the typed `lifecycle`
    /// field instead.
    pub fn to_legacy(&self) -> (&'static str, &'static str) {
        match self {
            Self::NotStarted => ("not_started", ""),
            Self::AwaitingTrust => ("awaiting_trust", ""),
            Self::AwaitingEnvBuild => ("awaiting_env_build", ""),
            Self::Resolving => ("starting", "resolving"),
            Self::PreparingEnv => ("starting", "preparing_env"),
            Self::Launching => ("starting", "launching"),
            Self::Connecting => ("starting", "connecting"),
            Self::Running(KernelActivity::Busy) => ("busy", ""),
            Self::Running(_) => ("idle", ""),
            Self::Error => ("error", ""),
            Self::Shutdown => ("shutdown", ""),
        }
    }
}

/// Read a [`RuntimeLifecycle`] from the CRDT, reconciling the typed
/// `kernel/lifecycle` + `kernel/activity` keys against the pre-typed
/// `kernel/status` + `kernel/starting_phase` pair.
///
/// Every in-repo writer now goes through the typed setters; this
/// fallback path matters only when reading a doc authored or mutated
/// by an older producer (captured test fixture, `from_doc` with raw
/// bytes, cross-version in-flight sync frame).
///
/// Resolution rule:
///
/// 1. No typed lifecycle key: derive from the string shape, or return
///    [`RuntimeLifecycle::NotStarted`] if that's empty too.
/// 2. Typed + string present: if the typed lifecycle's string
///    projection matches the actual `(status, starting_phase)` pair,
///    the two shapes agree — return the typed value. If they disagree,
///    a legacy-only writer ran more recently, so the string shape
///    wins.
/// 3. Typed key is unparseable (future variant, corruption): fall
///    through to the string shape so the real state isn't hidden.
pub fn resolve_lifecycle(
    lifecycle_key: &str,
    activity_key: &str,
    status: &str,
    starting_phase: &str,
) -> RuntimeLifecycle {
    if lifecycle_key.is_empty() {
        if status.is_empty() {
            return RuntimeLifecycle::NotStarted;
        }
        return RuntimeLifecycle::from_legacy(status, starting_phase);
    }
    let Some(typed) = RuntimeLifecycle::parse(lifecycle_key, activity_key) else {
        if status.is_empty() {
            return RuntimeLifecycle::NotStarted;
        }
        return RuntimeLifecycle::from_legacy(status, starting_phase);
    };
    // Both shapes present: whichever was written most recently wins.
    // Typed writers always clear the string keys too, so a mismatch
    // means a legacy-only writer ran after the last typed write.
    if status.is_empty() {
        return typed;
    }
    let (typed_status, typed_phase) = typed.to_legacy();
    if typed_status == status && typed_phase == starting_phase {
        typed
    } else {
        RuntimeLifecycle::from_legacy(status, starting_phase)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_as_str_round_trips() {
        assert_eq!(KernelActivity::Unknown.as_str(), "Unknown");
        assert_eq!(KernelActivity::Idle.as_str(), "Idle");
        assert_eq!(KernelActivity::Busy.as_str(), "Busy");
    }

    #[test]
    fn activity_parse_valid() {
        assert_eq!(
            KernelActivity::parse("Unknown"),
            Some(KernelActivity::Unknown)
        );
        assert_eq!(KernelActivity::parse("Idle"), Some(KernelActivity::Idle));
        assert_eq!(KernelActivity::parse("Busy"), Some(KernelActivity::Busy));
        assert_eq!(KernelActivity::parse("nope"), None);
        assert_eq!(KernelActivity::parse(""), None);
    }

    #[test]
    fn error_reason_as_str() {
        assert_eq!(
            KernelErrorReason::MissingIpykernel.as_str(),
            "missing_ipykernel"
        );
        assert_eq!(
            KernelErrorReason::CondaEnvYmlMissing.as_str(),
            "conda_env_yml_missing"
        );
    }

    #[test]
    fn error_reason_parse() {
        assert_eq!(
            KernelErrorReason::parse("missing_ipykernel"),
            Some(KernelErrorReason::MissingIpykernel)
        );
        assert_eq!(
            KernelErrorReason::parse("conda_env_yml_missing"),
            Some(KernelErrorReason::CondaEnvYmlMissing)
        );
        assert_eq!(KernelErrorReason::parse(""), None);
        assert_eq!(KernelErrorReason::parse("bogus"), None);
        // Parse is case-sensitive — the CRDT and legacy phase channel
        // both use exactly "missing_ipykernel".
        assert_eq!(KernelErrorReason::parse("Missing_Ipykernel"), None);
    }

    #[test]
    fn error_reason_as_str_round_trips_through_parse() {
        let reasons = [
            KernelErrorReason::MissingIpykernel,
            KernelErrorReason::CondaEnvYmlMissing,
        ];
        for r in reasons {
            assert_eq!(KernelErrorReason::parse(r.as_str()), Some(r));
        }
    }

    #[test]
    fn error_reason_serde_round_trip() -> Result<(), serde_json::Error> {
        // Variant-unit enums serialize as their variant name.
        let r = KernelErrorReason::MissingIpykernel;
        let json = serde_json::to_string(&r)?;
        assert_eq!(json, r#""MissingIpykernel""#);
        let back: KernelErrorReason = serde_json::from_str(&json)?;
        assert_eq!(back, r);
        Ok(())
    }

    #[test]
    fn lifecycle_variant_str() {
        use RuntimeLifecycle::*;
        assert_eq!(NotStarted.variant_str(), "NotStarted");
        assert_eq!(AwaitingTrust.variant_str(), "AwaitingTrust");
        assert_eq!(AwaitingEnvBuild.variant_str(), "AwaitingEnvBuild");
        assert_eq!(Resolving.variant_str(), "Resolving");
        assert_eq!(PreparingEnv.variant_str(), "PreparingEnv");
        assert_eq!(Launching.variant_str(), "Launching");
        assert_eq!(Connecting.variant_str(), "Connecting");
        assert_eq!(Running(KernelActivity::Idle).variant_str(), "Running");
        assert_eq!(Error.variant_str(), "Error");
        assert_eq!(Shutdown.variant_str(), "Shutdown");
    }

    #[test]
    fn lifecycle_parse_non_running_variants() {
        use RuntimeLifecycle::*;
        assert_eq!(RuntimeLifecycle::parse("NotStarted", ""), Some(NotStarted));
        assert_eq!(
            RuntimeLifecycle::parse("AwaitingTrust", ""),
            Some(AwaitingTrust)
        );
        assert_eq!(
            RuntimeLifecycle::parse("AwaitingEnvBuild", ""),
            Some(AwaitingEnvBuild)
        );
        assert_eq!(RuntimeLifecycle::parse("Resolving", ""), Some(Resolving));
        assert_eq!(
            RuntimeLifecycle::parse("PreparingEnv", ""),
            Some(PreparingEnv)
        );
        assert_eq!(RuntimeLifecycle::parse("Launching", ""), Some(Launching));
        assert_eq!(RuntimeLifecycle::parse("Connecting", ""), Some(Connecting));
        assert_eq!(RuntimeLifecycle::parse("Error", ""), Some(Error));
        assert_eq!(RuntimeLifecycle::parse("Shutdown", ""), Some(Shutdown));
        assert_eq!(RuntimeLifecycle::parse("bogus", ""), None);
    }

    #[test]
    fn lifecycle_parse_running_with_activity() {
        assert_eq!(
            RuntimeLifecycle::parse("Running", "Idle"),
            Some(RuntimeLifecycle::Running(KernelActivity::Idle)),
        );
        assert_eq!(
            RuntimeLifecycle::parse("Running", "Busy"),
            Some(RuntimeLifecycle::Running(KernelActivity::Busy)),
        );
        assert_eq!(
            RuntimeLifecycle::parse("Running", ""),
            Some(RuntimeLifecycle::Running(KernelActivity::Unknown)),
        );
        assert_eq!(
            RuntimeLifecycle::parse("Running", "bogus"),
            Some(RuntimeLifecycle::Running(KernelActivity::Unknown)),
        );
    }

    #[test]
    fn lifecycle_serde_tag_content() -> Result<(), serde_json::Error> {
        let running = RuntimeLifecycle::Running(KernelActivity::Busy);
        let json = serde_json::to_string(&running)?;
        assert_eq!(json, r#"{"lifecycle":"Running","activity":"Busy"}"#);
        let back: RuntimeLifecycle = serde_json::from_str(&json)?;
        assert_eq!(back, running);

        let not_started = RuntimeLifecycle::NotStarted;
        let json = serde_json::to_string(&not_started)?;
        assert_eq!(json, r#"{"lifecycle":"NotStarted"}"#);
        let back: RuntimeLifecycle = serde_json::from_str(&json)?;
        assert_eq!(back, not_started);
        Ok(())
    }

    #[test]
    fn lifecycle_default_is_not_started() {
        assert_eq!(RuntimeLifecycle::default(), RuntimeLifecycle::NotStarted);
    }

    // ── to_legacy projection (kept for wire-protocol callers) ──────

    #[test]
    fn to_legacy_non_running_variants() {
        use RuntimeLifecycle::*;
        assert_eq!(NotStarted.to_legacy(), ("not_started", ""));
        assert_eq!(AwaitingTrust.to_legacy(), ("awaiting_trust", ""));
        assert_eq!(AwaitingEnvBuild.to_legacy(), ("awaiting_env_build", ""));
        assert_eq!(Resolving.to_legacy(), ("starting", "resolving"));
        assert_eq!(PreparingEnv.to_legacy(), ("starting", "preparing_env"));
        assert_eq!(Launching.to_legacy(), ("starting", "launching"));
        assert_eq!(Connecting.to_legacy(), ("starting", "connecting"));
        assert_eq!(Error.to_legacy(), ("error", ""));
        assert_eq!(Shutdown.to_legacy(), ("shutdown", ""));
    }

    #[test]
    fn to_legacy_running_activity() {
        assert_eq!(
            RuntimeLifecycle::Running(KernelActivity::Idle).to_legacy(),
            ("idle", "")
        );
        assert_eq!(
            RuntimeLifecycle::Running(KernelActivity::Busy).to_legacy(),
            ("busy", "")
        );
        // Unknown has no legacy equivalent — falls back to "idle" because
        // the legacy shape interpreted anything non-busy as idle-ish.
        assert_eq!(
            RuntimeLifecycle::Running(KernelActivity::Unknown).to_legacy(),
            ("idle", "")
        );
    }

    // ── resolve_lifecycle ───────────────────────────────────────────

    #[test]
    fn resolve_parses_typed_keys() {
        assert_eq!(
            resolve_lifecycle("Running", "Idle", "", ""),
            RuntimeLifecycle::Running(KernelActivity::Idle)
        );
        assert_eq!(
            resolve_lifecycle("Launching", "", "", ""),
            RuntimeLifecycle::Launching
        );
        assert_eq!(
            resolve_lifecycle("Error", "", "", ""),
            RuntimeLifecycle::Error
        );
    }

    #[test]
    fn resolve_falls_back_to_string_shape_when_typed_is_absent() {
        // Pre-typed doc: only the string shape is populated. Callers
        // must still read running/busy/error kernels correctly, e.g.
        // when reading a captured fixture or a cross-version sync frame.
        assert_eq!(
            resolve_lifecycle("", "", "busy", ""),
            RuntimeLifecycle::Running(KernelActivity::Busy)
        );
        assert_eq!(
            resolve_lifecycle("", "", "starting", "launching"),
            RuntimeLifecycle::Launching
        );
        assert_eq!(
            resolve_lifecycle("", "", "error", ""),
            RuntimeLifecycle::Error
        );
    }

    #[test]
    fn resolve_defaults_on_empty_or_garbage() {
        // Both shapes absent: default to NotStarted. Unparseable typed
        // key with no string shape also defaults safely.
        assert_eq!(
            resolve_lifecycle("", "", "", ""),
            RuntimeLifecycle::NotStarted
        );
        assert_eq!(
            resolve_lifecycle("BogusFutureVariant", "Idle", "", ""),
            RuntimeLifecycle::NotStarted
        );
        // Unparseable typed + string shape: the string shape wins so
        // we don't silently hide the real state.
        assert_eq!(
            resolve_lifecycle("BogusFutureVariant", "Idle", "busy", ""),
            RuntimeLifecycle::Running(KernelActivity::Busy)
        );
    }

    #[test]
    fn resolve_running_unknown_preserved() {
        // Running with an unknown activity key parses as
        // Running(KernelActivity::Unknown) rather than falling back.
        assert_eq!(
            resolve_lifecycle("Running", "Unknown", "", ""),
            RuntimeLifecycle::Running(KernelActivity::Unknown)
        );
    }

    #[test]
    fn resolve_mixed_shape_prefers_legacy_string_when_shapes_disagree() {
        // Both shapes present but they describe different states. A
        // legacy-only writer (older producer, external mutation)
        // touched the string shape after the last typed write. Trust
        // the string shape so running/busy/error kernels aren't misread.
        assert_eq!(
            resolve_lifecycle("NotStarted", "", "busy", ""),
            RuntimeLifecycle::Running(KernelActivity::Busy)
        );
        assert_eq!(
            resolve_lifecycle("Running", "Idle", "starting", "launching"),
            RuntimeLifecycle::Launching
        );
    }

    #[test]
    fn resolve_mixed_shape_prefers_typed_when_shapes_agree() {
        // Typed Running(Idle) projects to ("idle", "") — matches the
        // legacy pair, so the two shapes agree and typed wins.
        assert_eq!(
            resolve_lifecycle("Running", "Idle", "idle", ""),
            RuntimeLifecycle::Running(KernelActivity::Idle)
        );
    }

    // ── ProjectFileKind ─────────────────────────────────────────────

    #[test]
    fn project_file_kind_as_str_round_trips_through_parse() {
        let kinds = [
            ProjectFileKind::PyprojectToml,
            ProjectFileKind::PixiToml,
            ProjectFileKind::EnvironmentYml,
        ];
        for k in kinds {
            assert_eq!(ProjectFileKind::parse(k.as_str()), Some(k));
        }
    }

    #[test]
    fn project_file_kind_rejects_unknown_strings() {
        assert_eq!(ProjectFileKind::parse(""), None);
        assert_eq!(ProjectFileKind::parse("PyprojectToml"), None); // case sensitive
        assert_eq!(ProjectFileKind::parse("bogus"), None);
    }

    // ── ProjectContext default / variant_str ────────────────────────

    #[test]
    fn project_context_default_is_pending() {
        assert_eq!(ProjectContext::default(), ProjectContext::Pending);
    }

    #[test]
    fn project_context_variant_str() {
        assert_eq!(ProjectContext::Pending.variant_str(), "pending");
        assert_eq!(
            ProjectContext::NotFound {
                observed_at: "t".into(),
            }
            .variant_str(),
            "not_found"
        );
        assert_eq!(
            ProjectContext::Detected {
                project_file: ProjectFile {
                    kind: ProjectFileKind::PyprojectToml,
                    absolute_path: "/abs/pyproject.toml".into(),
                    relative_to_notebook: "../pyproject.toml".into(),
                },
                parsed: ProjectFileParsed::default(),
                observed_at: "t".into(),
            }
            .variant_str(),
            "detected"
        );
        assert_eq!(
            ProjectContext::Unreadable {
                path: "/abs/pyproject.toml".into(),
                reason: "parse error".into(),
                observed_at: "t".into(),
            }
            .variant_str(),
            "unreadable"
        );
    }

    // ── ProjectContext serde round-trip ─────────────────────────────

    #[test]
    fn project_context_serde_tagged_round_trip() -> Result<(), serde_json::Error> {
        let detected = ProjectContext::Detected {
            project_file: ProjectFile {
                kind: ProjectFileKind::PyprojectToml,
                absolute_path: "/abs/pyproject.toml".into(),
                relative_to_notebook: "../pyproject.toml".into(),
            },
            parsed: ProjectFileParsed {
                dependencies: vec!["pandas>=2.0".into(), "numpy".into()],
                dev_dependencies: vec![],
                requires_python: Some(">=3.10".into()),
                prerelease: None,
                extras: ProjectFileExtras::None,
            },
            observed_at: "2026-04-25T12:00:00Z".into(),
        };
        let json = serde_json::to_string(&detected)?;
        assert!(json.contains(r#""state":"Detected""#));
        let back: ProjectContext = serde_json::from_str(&json)?;
        assert_eq!(back, detected);

        let not_found = ProjectContext::NotFound {
            observed_at: "2026-04-25T12:00:00Z".into(),
        };
        let json = serde_json::to_string(&not_found)?;
        assert!(json.contains(r#""state":"NotFound""#));
        let back: ProjectContext = serde_json::from_str(&json)?;
        assert_eq!(back, not_found);

        let pending = ProjectContext::Pending;
        let json = serde_json::to_string(&pending)?;
        assert_eq!(json, r#"{"state":"Pending"}"#);
        let back: ProjectContext = serde_json::from_str(&json)?;
        assert_eq!(back, pending);

        Ok(())
    }

    #[test]
    fn project_file_extras_pixi_round_trip() -> Result<(), serde_json::Error> {
        let extras = ProjectFileExtras::Pixi {
            channels: vec!["conda-forge".into()],
            pypi_dependencies: vec!["requests".into()],
        };
        let json = serde_json::to_string(&extras)?;
        let back: ProjectFileExtras = serde_json::from_str(&json)?;
        assert_eq!(back, extras);
        Ok(())
    }

    #[test]
    fn project_file_extras_default_is_none_variant() {
        assert_eq!(ProjectFileExtras::default(), ProjectFileExtras::None);
    }
}
