//! Daemon version tracking and reconnection message generation.

use runtimed_client::singleton::{read_daemon_info, DaemonInfo};
use std::path::PathBuf;
use tracing::info;

/// Read the current daemon version from the info file.
pub fn read_daemon_version(info_path: &PathBuf) -> Option<String> {
    read_daemon_info(info_path).map(|info| info.version)
}

/// Describe what happened during a reconnection for the MCP client.
#[derive(Debug, Clone)]
pub enum ReconnectionEvent {
    /// Daemon was upgraded to a new version.
    DaemonUpgrade {
        old_version: String,
        new_version: String,
        session_rejoined: bool,
    },
    /// Child restarted for non-upgrade reasons (crash, etc.).
    ChildRestart { session_rejoined: bool },
}

impl ReconnectionEvent {
    /// Generate a human-readable message for the MCP client.
    pub fn message(&self) -> String {
        match self {
            ReconnectionEvent::DaemonUpgrade {
                old_version,
                new_version,
                session_rejoined,
            } => {
                let session_msg = if *session_rejoined {
                    ", session reconnected"
                } else {
                    ""
                };
                format!("Daemon upgraded ({old_version} → {new_version}){session_msg}")
            }
            ReconnectionEvent::ChildRestart { session_rejoined } => {
                if *session_rejoined {
                    "MCP server restarted, session reconnected".to_string()
                } else {
                    "MCP server restarted".to_string()
                }
            }
        }
    }
}

/// Compare daemon versions before and after a child restart.
///
/// Returns `Some(ReconnectionEvent::DaemonUpgrade)` if the version changed,
/// `None` if versions are the same or we can't determine them.
pub fn detect_version_change(
    info_path: &PathBuf,
    old_version: Option<&str>,
) -> Option<ReconnectionEvent> {
    let new_version = read_daemon_version(info_path)?;
    let old = old_version?;

    if old != new_version {
        info!("Daemon version changed: {old} → {new_version}");
        Some(ReconnectionEvent::DaemonUpgrade {
            old_version: old.to_string(),
            new_version,
            session_rejoined: false, // caller updates this
        })
    } else {
        None
    }
}

/// Read the full daemon info (for logging, status reporting).
pub fn read_current_daemon_info(info_path: &PathBuf) -> Option<DaemonInfo> {
    read_daemon_info(info_path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn upgrade_message_with_session() {
        let event = ReconnectionEvent::DaemonUpgrade {
            old_version: "2.1.2".to_string(),
            new_version: "2.1.3".to_string(),
            session_rejoined: true,
        };
        assert_eq!(
            event.message(),
            "Daemon upgraded (2.1.2 → 2.1.3), session reconnected"
        );
    }

    #[test]
    fn upgrade_message_without_session() {
        let event = ReconnectionEvent::DaemonUpgrade {
            old_version: "2.1.2".to_string(),
            new_version: "2.1.3".to_string(),
            session_rejoined: false,
        };
        assert_eq!(event.message(), "Daemon upgraded (2.1.2 → 2.1.3)");
    }

    #[test]
    fn restart_message_with_session() {
        let event = ReconnectionEvent::ChildRestart {
            session_rejoined: true,
        };
        assert_eq!(event.message(), "MCP server restarted, session reconnected");
    }

    #[test]
    fn restart_message_without_session() {
        let event = ReconnectionEvent::ChildRestart {
            session_rejoined: false,
        };
        assert_eq!(event.message(), "MCP server restarted");
    }
}
