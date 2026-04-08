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
    use std::io::Write;

    // ── ReconnectionEvent::message() ──────────────────────────────────

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

    #[test]
    fn upgrade_message_with_build_metadata() {
        // Versions with git hashes (common in dev)
        let event = ReconnectionEvent::DaemonUpgrade {
            old_version: "2.1.2+abc1234".to_string(),
            new_version: "2.1.3+def5678".to_string(),
            session_rejoined: false,
        };
        assert_eq!(
            event.message(),
            "Daemon upgraded (2.1.2+abc1234 → 2.1.3+def5678)"
        );
    }

    #[test]
    fn upgrade_message_includes_arrow_character() {
        // Verify we use the unicode arrow, not ASCII
        let event = ReconnectionEvent::DaemonUpgrade {
            old_version: "1.0".to_string(),
            new_version: "2.0".to_string(),
            session_rejoined: false,
        };
        let msg = event.message();
        assert!(msg.contains('→'), "should use unicode arrow");
        assert!(!msg.contains("->"), "should not use ASCII arrow");
    }

    // ── detect_version_change() ───────────────────────────────────────

    #[test]
    fn detect_version_change_returns_none_for_missing_file() {
        let path = PathBuf::from("/nonexistent/daemon.json");
        assert!(detect_version_change(&path, Some("2.1.2")).is_none());
    }

    #[test]
    fn detect_version_change_returns_none_for_no_old_version() {
        let path = PathBuf::from("/nonexistent/daemon.json");
        assert!(detect_version_change(&path, None).is_none());
    }

    #[test]
    fn detect_version_change_with_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let info_path = dir.path().join("daemon.json");

        // Write a daemon info file with a different version
        let info = serde_json::json!({
            "endpoint": "/tmp/test.sock",
            "pid": 1234,
            "version": "2.1.3",
            "started_at": "2026-01-01T00:00:00Z"
        });
        let mut file = std::fs::File::create(&info_path).unwrap();
        write!(file, "{}", serde_json::to_string(&info).unwrap()).unwrap();

        // Version changed
        let result = detect_version_change(&info_path, Some("2.1.2"));
        assert!(result.is_some(), "should detect version change");
        match result.unwrap() {
            ReconnectionEvent::DaemonUpgrade {
                old_version,
                new_version,
                session_rejoined,
            } => {
                assert_eq!(old_version, "2.1.2");
                assert_eq!(new_version, "2.1.3");
                assert!(!session_rejoined, "caller should update this");
            }
            other => panic!("Expected DaemonUpgrade, got {other:?}"),
        }
    }

    #[test]
    fn detect_version_change_returns_none_when_same() {
        let dir = tempfile::tempdir().unwrap();
        let info_path = dir.path().join("daemon.json");

        let info = serde_json::json!({
            "endpoint": "/tmp/test.sock",
            "pid": 1234,
            "version": "2.1.2",
            "started_at": "2026-01-01T00:00:00Z"
        });
        let mut file = std::fs::File::create(&info_path).unwrap();
        write!(file, "{}", serde_json::to_string(&info).unwrap()).unwrap();

        // Same version
        assert!(
            detect_version_change(&info_path, Some("2.1.2")).is_none(),
            "should return None when versions match"
        );
    }

    // ── read_daemon_version() ─────────────────────────────────────────

    #[test]
    fn read_daemon_version_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let info_path = dir.path().join("daemon.json");

        let info = serde_json::json!({
            "endpoint": "/tmp/test.sock",
            "pid": 1234,
            "version": "2.1.3+abc",
            "started_at": "2026-01-01T00:00:00Z"
        });
        std::fs::write(&info_path, serde_json::to_string(&info).unwrap()).unwrap();

        assert_eq!(
            read_daemon_version(&info_path),
            Some("2.1.3+abc".to_string())
        );
    }

    #[test]
    fn read_daemon_version_returns_none_for_missing() {
        let path = PathBuf::from("/nonexistent/daemon.json");
        assert!(read_daemon_version(&path).is_none());
    }

    #[test]
    fn read_daemon_version_returns_none_for_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let info_path = dir.path().join("daemon.json");
        std::fs::write(&info_path, "not valid json").unwrap();
        assert!(read_daemon_version(&info_path).is_none());
    }

    // ── ReconnectionEvent cloning ─────────────────────────────────────

    #[test]
    fn reconnection_event_is_cloneable() {
        let event = ReconnectionEvent::DaemonUpgrade {
            old_version: "1.0".to_string(),
            new_version: "2.0".to_string(),
            session_rejoined: true,
        };
        let cloned = event.clone();
        assert_eq!(event.message(), cloned.message());
    }
}
