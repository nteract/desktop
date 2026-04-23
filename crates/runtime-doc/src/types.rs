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
}
