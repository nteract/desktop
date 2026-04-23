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

/// Lifecycle of a runtime, from not-started through running to shutdown.
///
/// Replaces the string-valued `KernelState.status` + `starting_phase` pair
/// with a typed sum. `Running` is the only variant that carries an
/// activity — it is impossible to represent a "busy kernel that hasn't
/// launched yet" in the type system. Error details are carried
/// out-of-band via `KernelState::error_reason` (not added in this phase)
/// so the enum stays `Eq`-able.
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
    /// Used when projecting to the future CRDT `kernel/lifecycle` string key
    /// and when bridging back to the legacy `(status, starting_phase)` pair.
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

    /// Derive a lifecycle from the legacy `(status, starting_phase)` string
    /// pair used by `KernelState`. Phase 1 uses this to populate
    /// `KernelState.lifecycle` without any CRDT schema change.
    pub fn from_legacy(status: &str, starting_phase: &str) -> Self {
        match status {
            "idle" => Self::Running(KernelActivity::Idle),
            "busy" => Self::Running(KernelActivity::Busy),
            "starting" => match starting_phase {
                "resolving" => Self::Resolving,
                "preparing_env" => Self::PreparingEnv,
                "launching" => Self::Launching,
                "connecting" => Self::Connecting,
                // Unknown or empty sub-phase — fall back to the first phase
                // so consumers still see "we're starting, somewhere in the
                // pipeline" rather than a default `NotStarted`.
                _ => Self::Resolving,
            },
            "error" => Self::Error,
            "shutdown" => Self::Shutdown,
            "awaiting_trust" => Self::AwaitingTrust,
            _ => Self::NotStarted,
        }
    }

    /// Project a lifecycle back to the `(status, starting_phase)` string
    /// pair. Used by the typed-shape writers to mirror into the string
    /// CRDT keys so readers that still consume the string shape see
    /// consistent state.
    ///
    /// This is the inverse of [`from_legacy`] with one caveat:
    /// `Running(KernelActivity::Unknown)` projects to `("idle", "")`
    /// because the string shape has no "unknown" status. Callers that
    /// care about the distinction should match on the typed `lifecycle`
    /// field rather than the string.
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

/// Reconcile the typed-shape and string-shape CRDT keys into a single
/// [`RuntimeLifecycle`].
///
/// During the transition window the same document can be mutated via two
/// setter families:
///
/// - **Typed setters** (`set_lifecycle`, `set_activity`,
///   `set_lifecycle_with_error`) write the typed keys AND mirror into the
///   string keys.
/// - **String setters** (`set_kernel_status`, `set_starting_phase`) write
///   only the string keys.
///
/// A doc that has only seen string setters still has its typed key at the
/// scaffold value `"NotStarted"`, so a naive "prefer typed" rule would
/// return [`RuntimeLifecycle::NotStarted`] even though the string shape
/// clearly says the kernel is busy. This function implements the
/// resolution rule:
///
/// 1. If `lifecycle_key` is empty (unscaffolded doc, pre-transition),
///    derive from the string pair via [`from_legacy`].
/// 2. Parse the typed pair. If the typed lifecycle's string projection
///    (via [`to_legacy`]) matches the actual `(status, starting_phase)`
///    pair, the two shapes agree — return the typed value.
/// 3. If they disagree, the string keys have been updated more recently
///    (typed setters always mirror, so a mismatch means a string-only
///    setter ran). Derive from the string pair.
///
/// The rule is "whichever shape was written most recently wins." It
/// relies on the writer-side invariant that typed setters mirror every
/// write into the string keys.
pub fn resolve_lifecycle(
    lifecycle_key: &str,
    activity_key: &str,
    status: &str,
    starting_phase: &str,
) -> RuntimeLifecycle {
    if lifecycle_key.is_empty() {
        return RuntimeLifecycle::from_legacy(status, starting_phase);
    }
    let Some(typed) = RuntimeLifecycle::parse(lifecycle_key, activity_key) else {
        return RuntimeLifecycle::from_legacy(status, starting_phase);
    };
    let (typed_status, typed_phase) = typed.to_legacy();
    if typed_status == status && typed_phase == starting_phase {
        typed
    } else {
        // String keys drifted from the typed projection — a string-only
        // setter ran after the last typed write, or this doc was
        // scaffolded by new() but only mutated through string setters.
        // Trust the string shape.
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

    #[test]
    fn lifecycle_from_legacy_idle_busy() {
        assert_eq!(
            RuntimeLifecycle::from_legacy("idle", ""),
            RuntimeLifecycle::Running(KernelActivity::Idle),
        );
        assert_eq!(
            RuntimeLifecycle::from_legacy("busy", ""),
            RuntimeLifecycle::Running(KernelActivity::Busy),
        );
    }

    #[test]
    fn lifecycle_from_legacy_starting_phases() {
        use RuntimeLifecycle::*;
        assert_eq!(
            RuntimeLifecycle::from_legacy("starting", "resolving"),
            Resolving
        );
        assert_eq!(
            RuntimeLifecycle::from_legacy("starting", "preparing_env"),
            PreparingEnv
        );
        assert_eq!(
            RuntimeLifecycle::from_legacy("starting", "launching"),
            Launching
        );
        assert_eq!(
            RuntimeLifecycle::from_legacy("starting", "connecting"),
            Connecting
        );
        // Empty phase falls back to the first phase so the UI still reads
        // "we're starting" rather than "not started."
        assert_eq!(RuntimeLifecycle::from_legacy("starting", ""), Resolving);
    }

    #[test]
    fn lifecycle_from_legacy_terminal_states() {
        use RuntimeLifecycle::*;
        assert_eq!(RuntimeLifecycle::from_legacy("error", ""), Error);
        assert_eq!(RuntimeLifecycle::from_legacy("shutdown", ""), Shutdown);
        assert_eq!(
            RuntimeLifecycle::from_legacy("awaiting_trust", ""),
            AwaitingTrust
        );
        assert_eq!(RuntimeLifecycle::from_legacy("not_started", ""), NotStarted);
        assert_eq!(RuntimeLifecycle::from_legacy("", ""), NotStarted);
        assert_eq!(RuntimeLifecycle::from_legacy("gibberish", ""), NotStarted);
    }

    // ── Phase 2: to_legacy projection ───────────────────────────────

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

    #[test]
    fn from_legacy_to_legacy_round_trip_is_lossy_for_unknown() {
        // Running(Unknown) → ("idle", "") → Running(Idle). The test pins
        // the loss so future work that might try to preserve Unknown
        // through the legacy channel surfaces here.
        let lc = RuntimeLifecycle::Running(KernelActivity::Unknown);
        let (status, phase) = lc.to_legacy();
        assert_eq!(
            RuntimeLifecycle::from_legacy(status, phase),
            RuntimeLifecycle::Running(KernelActivity::Idle)
        );
    }

    #[test]
    fn to_legacy_from_legacy_round_trip_preserves_non_running() {
        use RuntimeLifecycle::*;
        for lc in [
            NotStarted,
            AwaitingTrust,
            Resolving,
            PreparingEnv,
            Launching,
            Connecting,
            Running(KernelActivity::Idle),
            Running(KernelActivity::Busy),
            Error,
            Shutdown,
        ] {
            let (status, phase) = lc.to_legacy();
            let round_tripped = RuntimeLifecycle::from_legacy(status, phase);
            assert_eq!(
                round_tripped, lc,
                "round-trip changed {lc:?} via ({status:?}, {phase:?}) → {round_tripped:?}"
            );
        }
    }

    // ── resolve_lifecycle ───────────────────────────────────────────

    #[test]
    fn resolve_prefers_typed_when_shapes_agree() {
        // Typed shape "Running" + "Idle" projects to ("idle", "").
        // Matches the string shape → typed wins.
        assert_eq!(
            resolve_lifecycle("Running", "Idle", "idle", ""),
            RuntimeLifecycle::Running(KernelActivity::Idle)
        );
    }

    #[test]
    fn resolve_falls_back_to_string_when_typed_is_scaffold_default() {
        // Scaffolded doc with only a string setter run: typed key is
        // still "NotStarted", string key says "idle". The mismatch is
        // what we detect — string shape wins.
        assert_eq!(
            resolve_lifecycle("NotStarted", "", "idle", ""),
            RuntimeLifecycle::Running(KernelActivity::Idle)
        );
    }

    #[test]
    fn resolve_empty_typed_key_means_pre_scaffold_doc() {
        // Doc constructed via new_empty() has no kernel map at all, so
        // read_str returns "". Fall straight through to the string
        // derivation without parsing.
        assert_eq!(
            resolve_lifecycle("", "", "busy", ""),
            RuntimeLifecycle::Running(KernelActivity::Busy)
        );
        assert_eq!(
            resolve_lifecycle("", "", "not_started", ""),
            RuntimeLifecycle::NotStarted
        );
    }

    #[test]
    fn resolve_falls_back_when_typed_disagrees_with_string() {
        // Typed says "Running"/"Idle" but string says "busy". A string
        // setter ran after the last typed mirror — trust the string.
        assert_eq!(
            resolve_lifecycle("Running", "Idle", "busy", ""),
            RuntimeLifecycle::Running(KernelActivity::Busy)
        );
    }

    #[test]
    fn resolve_falls_back_on_unparseable_typed_key() {
        // Garbage in the typed lifecycle key (future variant? corruption?)
        // falls through to the string derivation rather than returning
        // Default and hiding the real state.
        assert_eq!(
            resolve_lifecycle("BogusFutureVariant", "Idle", "busy", ""),
            RuntimeLifecycle::Running(KernelActivity::Busy)
        );
    }

    #[test]
    fn resolve_treats_running_unknown_as_agreeing_with_idle_string() {
        // Running(Unknown) projects to ("idle", "") because the string
        // shape has no "unknown" equivalent. A string status of "idle"
        // agrees with that projection, so the typed Running(Unknown)
        // wins and the Unknown activity is preserved.
        assert_eq!(
            resolve_lifecycle("Running", "Unknown", "idle", ""),
            RuntimeLifecycle::Running(KernelActivity::Unknown)
        );
    }

    #[test]
    fn resolve_typed_starting_matches_string_starting() {
        // Typed "Launching" projects to ("starting", "launching"). If the
        // string pair matches, typed wins.
        assert_eq!(
            resolve_lifecycle("Launching", "", "starting", "launching"),
            RuntimeLifecycle::Launching
        );
    }

    #[test]
    fn resolve_typed_starting_disagrees_on_phase() {
        // Typed "Launching" projects to ("starting", "launching") but
        // string phase says "connecting". String shape wins.
        assert_eq!(
            resolve_lifecycle("Launching", "", "starting", "connecting"),
            RuntimeLifecycle::Connecting
        );
    }

    #[test]
    fn resolve_error_with_empty_reason_agrees() {
        // Error has no string phase component; both shapes agree.
        assert_eq!(
            resolve_lifecycle("Error", "", "error", ""),
            RuntimeLifecycle::Error
        );
    }
}
