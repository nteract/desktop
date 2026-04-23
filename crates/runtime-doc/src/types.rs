use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct StreamOutputState {
    pub index: usize,
    pub blob_hash: String,
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
}

impl KernelErrorReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingIpykernel => "missing_ipykernel",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "missing_ipykernel" => Some(Self::MissingIpykernel),
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
    }

    #[test]
    fn error_reason_parse() {
        assert_eq!(
            KernelErrorReason::parse("missing_ipykernel"),
            Some(KernelErrorReason::MissingIpykernel)
        );
        assert_eq!(KernelErrorReason::parse(""), None);
        assert_eq!(KernelErrorReason::parse("bogus"), None);
        // Parse is case-sensitive — the CRDT and legacy phase channel
        // both use exactly "missing_ipykernel".
        assert_eq!(KernelErrorReason::parse("Missing_Ipykernel"), None);
    }

    #[test]
    fn error_reason_as_str_round_trips_through_parse() {
        let reasons = [KernelErrorReason::MissingIpykernel];
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
}
