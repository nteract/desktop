//! nteract-mcp — resilient MCP server for nteract.
//!
//! Ships as a sidecar in the nteract desktop app, inside the `.mcpb`
//! Claude Desktop extension, and in the Claude Code plugin. Finds
//! `runt` via `runt-workspace`, spawns `runt mcp` as a child, and
//! proxies MCP over stdio with transparent restart on child death
//! (daemon upgrade, crash, etc.).

// Allow `expect()` and `unwrap()` in tests
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;

use rmcp::ServiceExt;
use runt_mcp_proxy::{McpProxy, ProxyConfig};
use tracing::{error, info, warn};

fn selected_build_channel(channel: &str) -> runt_workspace::BuildChannel {
    if channel.eq_ignore_ascii_case("nightly") {
        runt_workspace::BuildChannel::Nightly
    } else {
        runt_workspace::BuildChannel::Stable
    }
}

fn runt_binary_name_for_channel(channel: &str) -> &'static str {
    runt_workspace::cli_command_name_for(selected_build_channel(channel))
}

/// Find the `runt` or `runt-nightly` binary on PATH or in platform-specific locations.
fn find_runt_binary(channel: &str) -> Option<PathBuf> {
    let build_channel = selected_build_channel(channel);
    let binary_name = runt_workspace::cli_command_name_for(build_channel);

    // 1. Check PATH via `which`
    let which_cmd = if cfg!(target_os = "windows") {
        "where"
    } else {
        "which"
    };

    if let Ok(output) = std::process::Command::new(which_cmd)
        .arg(binary_name)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }

    // 2. Check platform-specific app bundle locations
    #[cfg(target_os = "macos")]
    {
        let build_channel = if channel == "nightly" {
            runt_workspace::BuildChannel::Nightly
        } else {
            runt_workspace::BuildChannel::Stable
        };

        if let Some(app_bundle) = runt_workspace::find_installed_app_bundle_for(build_channel) {
            let binary = app_bundle.join("Contents/MacOS").join(binary_name);
            if binary.exists() {
                return Some(binary);
            }
        }
    }

    // 3. Check well-known install locations
    if let Some(home) = dirs::home_dir() {
        #[cfg(target_os = "linux")]
        {
            let local_bin = home.join(".local/bin").join(binary_name);
            if local_bin.exists() {
                return Some(local_bin);
            }

            // Check packaged app locations (/usr/share/<app>/, /opt/<app>/)
            let build_channel = if channel == "nightly" {
                runt_workspace::BuildChannel::Nightly
            } else {
                runt_workspace::BuildChannel::Stable
            };
            for app_name in runt_workspace::desktop_app_launch_candidates_for(build_channel) {
                let slug = app_name.to_lowercase().replace(' ', "-");
                let usr_share =
                    std::path::PathBuf::from(format!("/usr/share/{slug}/{binary_name}"));
                if usr_share.exists() {
                    return Some(usr_share);
                }
                let opt = std::path::PathBuf::from(format!("/opt/{slug}/{binary_name}"));
                if opt.exists() {
                    return Some(opt);
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            if let Some(local_app_data) = dirs::data_local_dir() {
                let app_name = if channel == "nightly" {
                    "nteract Nightly"
                } else {
                    "nteract"
                };
                let exe_name = format!("{binary_name}.exe");
                let candidate = local_app_data.join(app_name).join(&exe_name);
                if candidate.exists() {
                    return Some(candidate);
                }
                let candidate = local_app_data
                    .join("Programs")
                    .join(app_name)
                    .join(&exe_name);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }

        // Suppress unused variable warning on macOS where home is only used
        // in the linux/windows cfg blocks
        let _ = home;
    }

    None
}

fn child_env_for_channel(channel: &str) -> HashMap<String, String> {
    HashMap::from([("NTERACT_CHANNEL".to_string(), channel.to_string())])
}

fn proxy_cache_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| {
        let dir = d.join(runt_workspace::cache_namespace()).join("proxy");
        let _ = std::fs::create_dir_all(&dir);
        dir
    })
}

fn proxy_config_for_channel(channel: String) -> ProxyConfig {
    let child_env = child_env_for_channel(&channel);
    let binary_name = runt_binary_name_for_channel(&channel);
    let channel_for_resolve = channel.clone();
    ProxyConfig {
        resolve_child_command: Box::new(move || {
            find_runt_binary(&channel_for_resolve).ok_or_else(|| {
                format!("{binary_name} no longer found on PATH or in known install locations")
            })
        }),
        child_args: vec!["mcp".to_string()],
        child_env,
        server_name: runt_workspace::desktop_product_name().to_string(),
        cache_dir: proxy_cache_dir(),
        monitor_poll_interval_ms: 500,
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    // Auto-reap child processes. The proxy spawns `runt mcp` children via
    // rmcp's TokioChildProcess; when a child exits (e.g. daemon upgrade
    // exit code 75), the tokio Child handle is dropped without waitpid(),
    // leaving a zombie. SIG_IGN tells the kernel to auto-reap so zombies
    // never accumulate across long MCP sessions.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGCHLD, libc::SIG_IGN);
    }

    // Log to stderr (MCP uses stdout for transport)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    // Channel is baked in at compile time via RUNT_BUILD_CHANNEL (read by
    // runt_workspace::build_channel()). The NTERACT_CHANNEL env var is an
    // optional runtime override for testing — it does NOT change the binary
    // identity, just which child binary is discovered.
    let channel = std::env::var("NTERACT_CHANNEL")
        .unwrap_or_else(|_| runt_workspace::channel_display_name().to_string());
    let app_name = runt_workspace::desktop_display_name();
    let binary_name = runt_binary_name_for_channel(&channel);

    info!(
        "nteract-mcp starting (channel={channel}, compiled={})",
        runt_workspace::channel_display_name()
    );
    if channel != runt_workspace::channel_display_name() {
        warn!(
            "NTERACT_CHANNEL overrides baked-in channel ({} → {channel})",
            runt_workspace::channel_display_name()
        );
    }
    if find_runt_binary(&channel).is_none() {
        eprintln!(
            "Error: {binary_name} not found.\n\n\
             Install {app_name} from https://nteract.io to use this MCP server.\n\
             The app puts {binary_name} on your PATH during installation."
        );
        return ExitCode::FAILURE;
    }
    info!("Validated {binary_name} is available");

    // Pass resolution as a closure so the proxy re-discovers the binary
    // on every child restart. This is the core upgrade mechanism: the user
    // upgrades the nteract app (new runt binary), and the proxy picks it up
    // without needing to reinstall the MCPB extension.
    //
    // Daemon version tracking now flows through the child's MCP handshake
    // (`runt mcp` stamps the daemon version into `ServerInfo.title`), so the
    // proxy no longer opens its own daemon socket — that used to drag the
    // full runtimed-client compile graph into every MCP process.
    let config = proxy_config_for_channel(channel.clone());

    let proxy = McpProxy::new(config, None);

    // Start the MCP server on stdio immediately
    let transport = rmcp::transport::io::stdio();
    let server = match proxy.serve(transport).await {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to start MCP server: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Extract upstream client identity
    let (upstream_name, upstream_title) = server
        .peer()
        .peer_info()
        .map(|info| {
            let name = info.client_info.name.clone();
            let title = info.client_info.title.clone();
            info!("Upstream MCP client: name={name:?}, title={title:?}");
            (name, title)
        })
        .unwrap_or_else(|| ("unknown".to_string(), None));

    let proxy_ref = server.service().clone();
    proxy_ref
        .set_upstream_identity(upstream_name, upstream_title)
        .await;

    // Child spawn and `tools/list_changed` / `resources/list_changed` notifications
    // happen in `McpProxy::on_initialized`, after the client sends
    // `notifications/initialized`. Firing them earlier races the MCP handshake —
    // Claude Code drops notifications received before it has finished initializing,
    // leaving its tool list empty.

    // Wait for client disconnect OR exit signal from incompatible tool divergence.
    let exit_signal = proxy_ref.exit_signal.clone();
    let cancel_token = server.cancellation_token();
    tokio::select! {
        result = server.waiting() => {
            match result {
                Ok(reason) => info!("Shutting down: {reason:?}"),
                Err(e) => error!("Server error: {e}"),
            }
        }
        _ = exit_signal.notified() => {
            info!(
                "Exiting due to incompatible tool list change after daemon upgrade. \
                 The MCP client will restart us with the updated tools."
            );
            cancel_token.cancel();
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }

    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_name_selects_the_expected_child_binary() {
        assert_eq!(runt_binary_name_for_channel("stable"), "runt");
        assert_eq!(runt_binary_name_for_channel("nightly"), "runt-nightly");
        assert_eq!(runt_binary_name_for_channel("NIGHTLY"), "runt-nightly");
        assert_eq!(runt_binary_name_for_channel("future"), "runt");
    }

    #[test]
    fn child_env_is_limited_to_the_channel_contract() {
        let env = child_env_for_channel("stable");
        assert_eq!(env.len(), 1);
        assert_eq!(env.get("NTERACT_CHANNEL"), Some(&"stable".to_string()));
        assert!(!env.contains_key("RUNTIMED_DEV"));
        assert!(!env.contains_key("RUNTIMED_WORKSPACE_PATH"));
        assert!(!env.contains_key("RUNTIMED_SOCKET_PATH"));
    }

    #[test]
    fn proxy_config_uses_the_resilient_runt_mcp_child_contract() {
        let config = proxy_config_for_channel("nightly".to_string());

        assert_eq!(config.child_args, vec!["mcp".to_string()]);
        assert_eq!(
            config.child_env.get("NTERACT_CHANNEL"),
            Some(&"nightly".to_string())
        );
        assert_eq!(config.server_name, runt_workspace::desktop_product_name());
        assert_eq!(config.monitor_poll_interval_ms, 500);
    }

    #[test]
    fn proxy_cache_dir_is_under_the_channel_cache_namespace() {
        let Some(dir) = proxy_cache_dir() else {
            return;
        };

        let suffix = PathBuf::from(runt_workspace::cache_namespace()).join("proxy");
        assert!(dir.ends_with(suffix));
    }
}
