use std::sync::Arc;

use crate::daemon::Daemon;
use crate::task_supervisor::spawn_best_effort;

pub fn spawn_daemon_heartbeat(daemon: Arc<Daemon>) {
    spawn_best_effort("telemetry-heartbeat", async move {
        daemon_heartbeat_loop(daemon).await;
    });
}

async fn daemon_heartbeat_loop(daemon: Arc<Daemon>) {
    if runtimed_client::telemetry::is_telemetry_suppressed() {
        tracing::debug!("[telemetry] suppressed, skipping daemon heartbeat loop");
        return;
    }

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("[telemetry] failed to build HTTP client: {e}");
            return;
        }
    };

    loop {
        try_send_daemon_heartbeat(&daemon, &client).await;
        tokio::time::sleep(std::time::Duration::from_secs(60 * 60)).await;
    }
}

async fn try_send_daemon_heartbeat(daemon: &Arc<Daemon>, client: &reqwest::Client) {
    let (settings, install_id) = {
        let mut settings_doc = daemon.settings.write().await;
        let id = runtimed_client::settings_doc::ensure_install_id(&mut settings_doc);
        let snapshot = settings_doc.get_all();
        (snapshot, id)
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if !runtimed_client::telemetry::should_send(
        settings.telemetry_enabled,
        settings.onboarding_completed,
        settings.telemetry_last_daemon_ping_at,
        now,
    ) {
        return;
    }

    let Some(platform) = runtimed_client::telemetry::detect_platform() else {
        return;
    };
    let Some(arch) = runtimed_client::telemetry::detect_arch() else {
        return;
    };

    let payload = runtimed_client::telemetry::HeartbeatPayload {
        install_id,
        source: "daemon".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        channel: runtimed_client::telemetry::detect_channel().to_string(),
        platform: platform.to_string(),
        arch: arch.to_string(),
    };

    match runtimed_client::telemetry::send_heartbeat(client, &payload).await {
        Ok(()) => tracing::info!("[telemetry] sent daemon heartbeat"),
        Err(e) => tracing::warn!("[telemetry] daemon heartbeat failed: {e}"),
    }

    // Update timestamp on both success and failure
    let mut settings_doc = daemon.settings.write().await;
    settings_doc.put_u64("telemetry_last_daemon_ping_at", now);
}
