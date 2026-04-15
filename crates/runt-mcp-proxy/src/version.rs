//! Reconnection event messages emitted on child restart / daemon upgrade.
//!
//! Version detection itself lives on the `McpProxy` via its long-lived
//! `DaemonConnection` (see `crate::proxy`). This module now only defines
//! the user-facing `ReconnectionEvent` enum and the text shown to MCP
//! clients when they reconnect.

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

    #[test]
    fn upgrade_message_with_build_metadata() {
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
        let event = ReconnectionEvent::DaemonUpgrade {
            old_version: "1.0".to_string(),
            new_version: "2.0".to_string(),
            session_rejoined: false,
        };
        let msg = event.message();
        assert!(msg.contains('→'));
        assert!(!msg.contains("->"));
    }

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
