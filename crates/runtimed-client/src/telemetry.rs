use serde::Serialize;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_TELEMETRY_ENDPOINT: &str = "https://telemetry.runtimed.com/v1/ping";
const SEND_TIMEOUT: Duration = Duration::from_secs(3);
const THROTTLE_SECS: u64 = 20 * 60 * 60; // 20 hours
const LOOP_INTERVAL: Duration = Duration::from_secs(60 * 60); // 1 hour

fn endpoint() -> String {
    std::env::var("NTERACT_TELEMETRY_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_TELEMETRY_ENDPOINT.to_string())
}

#[derive(Debug, Clone, Serialize)]
pub struct HeartbeatPayload {
    pub install_id: String,
    pub source: String,
    pub version: String,
    pub channel: String,
    pub platform: String,
    pub arch: String,
}

pub fn detect_channel() -> &'static str {
    match option_env!("RUNT_BUILD_CHANNEL") {
        Some("stable") => "stable",
        _ => "nightly",
    }
}

pub fn detect_platform() -> Option<&'static str> {
    match std::env::consts::OS {
        "macos" => Some("macos"),
        "linux" => Some("linux"),
        "windows" => Some("windows"),
        _ => None,
    }
}

pub fn detect_arch() -> Option<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Some("x86_64"),
        "aarch64" => Some("arm64"),
        _ => None,
    }
}

pub fn is_telemetry_suppressed() -> bool {
    if crate::is_dev_mode() {
        return true;
    }
    if std::env::var("CI").is_ok() {
        return true;
    }
    if std::env::var("NTERACT_TELEMETRY_DISABLE").is_ok() {
        return true;
    }
    false
}

/// Superset of [`should_send`] that also requires explicit user consent.
///
/// Returns true only when the consent-recorded flag is set AND every other
/// gate in `should_send` passes. This is the gate `try_send` actually uses;
/// `should_send` is preserved as a thin API for callers that don't carry
/// the consent flag yet.
pub fn should_send_full(
    telemetry_enabled: bool,
    onboarding_completed: bool,
    consent_recorded: bool,
    last_ping_at: Option<u64>,
    now_secs: u64,
) -> bool {
    if !consent_recorded {
        return false;
    }
    should_send(
        telemetry_enabled,
        onboarding_completed,
        last_ping_at,
        now_secs,
    )
}

pub fn should_send(
    telemetry_enabled: bool,
    onboarding_completed: bool,
    last_ping_at: Option<u64>,
    now_secs: u64,
) -> bool {
    if !telemetry_enabled {
        return false;
    }
    if !onboarding_completed {
        return false;
    }
    if detect_platform().is_none() || detect_arch().is_none() {
        return false;
    }
    if let Some(last) = last_ping_at {
        if now_secs.saturating_sub(last) < THROTTLE_SECS {
            return false;
        }
    }
    true
}

/// Superset of [`blocking_gates`] that also reports a "consent not recorded"
/// gate when the user has not yet pressed an onboarding CTA.
pub fn blocking_gates_full(
    telemetry_enabled: bool,
    onboarding_completed: bool,
    consent_recorded: bool,
    last_ping_at: Option<u64>,
    now_secs: u64,
) -> Vec<&'static str> {
    let mut gates = blocking_gates(
        telemetry_enabled,
        onboarding_completed,
        last_ping_at,
        now_secs,
    );
    if !consent_recorded {
        gates.push("consent not recorded");
    }
    gates
}

pub fn blocking_gates(
    telemetry_enabled: bool,
    onboarding_completed: bool,
    last_ping_at: Option<u64>,
    now_secs: u64,
) -> Vec<&'static str> {
    let mut gates = Vec::new();
    if crate::is_dev_mode() {
        gates.push("dev mode (RUNTIMED_DEV=1 or RUNTIMED_WORKSPACE_PATH set)");
    }
    if std::env::var("CI").is_ok() {
        gates.push("CI environment detected");
    }
    if std::env::var("NTERACT_TELEMETRY_DISABLE").is_ok() {
        gates.push("NTERACT_TELEMETRY_DISABLE is set");
    }
    if !telemetry_enabled {
        gates.push("telemetry_enabled = false");
    }
    if !onboarding_completed {
        gates.push("onboarding not completed");
    }
    if detect_platform().is_none() {
        gates.push("unsupported platform");
    }
    if detect_arch().is_none() {
        gates.push("unsupported architecture");
    }
    if let Some(last) = last_ping_at {
        if now_secs.saturating_sub(last) < THROTTLE_SECS {
            gates.push("throttled (last ping < 20h ago)");
        }
    }
    gates
}

pub async fn send_heartbeat(
    client: &reqwest::Client,
    payload: &HeartbeatPayload,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let body = serde_json::to_vec(payload)?;
    let resp = client
        .post(endpoint())
        .header("Content-Type", "application/json")
        .timeout(SEND_TIMEOUT)
        .body(body)
        .send()
        .await?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("telemetry ping returned {}", resp.status()).into())
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Shared heartbeat loop for long-running processes (daemon, MCP).
///
/// Checks every hour, sends if >20h since last ping. Reads and writes
/// settings through the daemon's Automerge sync path when available,
/// falling back to disk JSON.
pub async fn heartbeat_loop(source: &str, timestamp_key: &str) {
    if is_telemetry_suppressed() {
        log::debug!("[telemetry] suppressed for source={source}, skipping heartbeat loop");
        return;
    }

    let client = match reqwest::Client::builder().timeout(SEND_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            log::warn!("[telemetry] failed to build HTTP client: {e}");
            return;
        }
    };

    loop {
        try_send(&client, source, timestamp_key).await;
        tokio::time::sleep(LOOP_INTERVAL).await;
    }
}

/// Single-shot heartbeat for processes that don't need a loop (e.g. the app).
pub async fn heartbeat_once(source: &str, timestamp_key: &str) {
    if is_telemetry_suppressed() {
        log::debug!("[telemetry] suppressed for source={source}, skipping heartbeat");
        return;
    }

    let client = match reqwest::Client::builder().timeout(SEND_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            log::warn!("[telemetry] failed to build HTTP client: {e}");
            return;
        }
    };

    try_send(&client, source, timestamp_key).await;
}

async fn try_send(client: &reqwest::Client, source: &str, timestamp_key: &str) {
    let settings = match read_settings().await {
        Some(s) => s,
        None => return,
    };

    let now = now_secs();
    let last_ping_at = match timestamp_key {
        "telemetry_last_daemon_ping_at" => settings.telemetry_last_daemon_ping_at,
        "telemetry_last_app_ping_at" => settings.telemetry_last_app_ping_at,
        "telemetry_last_mcp_ping_at" => settings.telemetry_last_mcp_ping_at,
        _ => None,
    };

    if !should_send_full(
        settings.telemetry_enabled,
        settings.onboarding_completed,
        settings.telemetry_consent_recorded,
        last_ping_at,
        now,
    ) {
        return;
    }

    let install_id = if settings.install_id.is_empty() {
        let id = uuid::Uuid::new_v4().to_string();
        write_setting("install_id", &serde_json::Value::String(id.clone())).await;
        id
    } else {
        settings.install_id
    };

    let Some(platform) = detect_platform() else {
        return;
    };
    let Some(arch) = detect_arch() else {
        return;
    };

    let payload = HeartbeatPayload {
        install_id,
        source: source.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        channel: detect_channel().to_string(),
        platform: platform.to_string(),
        arch: arch.to_string(),
    };

    match send_heartbeat(client, &payload).await {
        Ok(()) => log::info!("[telemetry] sent heartbeat source={source}"),
        Err(e) => log::warn!("[telemetry] heartbeat failed source={source}: {e}"),
    }

    // Update timestamp on both success and failure to prevent retry storms
    write_setting(timestamp_key, &serde_json::Value::Number(now.into())).await;
}

async fn read_settings() -> Option<crate::settings_doc::SyncedSettings> {
    match crate::sync_client::try_get_synced_settings().await {
        Ok(settings) => Some(settings),
        Err(_) => {
            let path = crate::settings_json_path();
            let contents = std::fs::read_to_string(&path).ok()?;
            serde_json::from_str(&contents).ok()
        }
    }
}

async fn write_setting(key: &str, value: &serde_json::Value) {
    let socket_path = crate::default_socket_path();
    match crate::sync_client::SyncClient::connect_with_timeout(
        socket_path,
        Duration::from_millis(500),
    )
    .await
    {
        Ok(mut client) => {
            if let Err(e) = client.put_value(key, value).await {
                log::debug!("[telemetry] failed to write {key} via daemon: {e}");
            }
        }
        Err(e) => {
            log::debug!("[telemetry] daemon unavailable for write {key}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_payload_shape() {
        let payload = HeartbeatPayload {
            install_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            source: "daemon".to_string(),
            version: "1.2.3".to_string(),
            channel: "nightly".to_string(),
            platform: "macos".to_string(),
            arch: "arm64".to_string(),
        };
        let json: serde_json::Value = serde_json::to_value(&payload).unwrap();
        let obj = json.as_object().unwrap();
        assert_eq!(obj.len(), 6);
        assert!(obj.contains_key("install_id"));
        assert!(obj.contains_key("source"));
        assert!(obj.contains_key("version"));
        assert!(obj.contains_key("channel"));
        assert!(obj.contains_key("platform"));
        assert!(obj.contains_key("arch"));
    }

    #[test]
    fn test_channel_detection() {
        let ch = detect_channel();
        assert!(ch == "stable" || ch == "nightly");
    }

    #[test]
    fn test_platform_detection() {
        let p = detect_platform();
        assert!(p.is_some(), "tests must run on a supported platform");
    }

    #[test]
    fn test_arch_detection() {
        let a = detect_arch();
        assert!(a.is_some(), "tests must run on a supported arch");
    }

    #[test]
    fn test_should_send_happy_path() {
        assert!(should_send(true, true, None, 1000));
    }

    #[test]
    fn test_should_send_disabled() {
        assert!(!should_send(false, true, None, 1000));
    }

    #[test]
    fn test_should_send_not_onboarded() {
        assert!(!should_send(true, false, None, 1000));
    }

    #[test]
    fn test_should_send_throttled() {
        let now = 1_700_000_000u64;
        let recent = now - (19 * 60 * 60); // 19 hours ago
        assert!(!should_send(true, true, Some(recent), now));
    }

    #[test]
    fn test_should_send_past_throttle() {
        let now = 1_700_000_000u64;
        let old = now - (21 * 60 * 60); // 21 hours ago
        assert!(should_send(true, true, Some(old), now));
    }

    #[test]
    fn test_blocking_gates_all_clear() {
        // Can't fully test env-dependent gates in unit tests, but we can
        // test the settings-based gates
        let gates = blocking_gates(true, true, None, 1000);
        // At minimum, settings-based gates should not fire
        assert!(!gates.contains(&"telemetry_enabled = false"));
        assert!(!gates.contains(&"onboarding not completed"));
        assert!(!gates.contains(&"throttled (last ping < 20h ago)"));
    }

    #[test]
    fn test_blocking_gates_disabled() {
        let gates = blocking_gates(false, true, None, 1000);
        assert!(gates.contains(&"telemetry_enabled = false"));
    }

    #[test]
    fn test_blocking_gates_throttled() {
        let now = 1_700_000_000u64;
        let recent = now - (10 * 60 * 60);
        let gates = blocking_gates(true, true, Some(recent), now);
        assert!(gates.contains(&"throttled (last ping < 20h ago)"));
    }

    #[test]
    fn test_should_send_requires_consent_recorded() {
        // Everything green except consent_recorded = false.
        assert!(!should_send_full(true, true, false, None, 1000));
    }

    #[test]
    fn test_should_send_with_all_true() {
        assert!(should_send_full(true, true, true, None, 1000));
    }

    #[test]
    fn test_blocking_gates_consent_not_recorded() {
        let gates = blocking_gates_full(true, true, false, None, 1000);
        assert!(gates.contains(&"consent not recorded"));
    }
}
