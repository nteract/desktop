//! mcpb-runt — resilient MCP proxy for the nteract MCPB bundle.
//!
//! Ships inside the `.mcpb` archive. Finds `runt` via `runt-workspace`,
//! spawns `runt mcp` as a child, and proxies MCP over stdio with transparent
//! restart on child death (daemon upgrade, crash, etc.).

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;

use rmcp::ServiceExt;
use runt_mcp_proxy::{McpProxy, ProxyConfig};
use tracing::{error, info};

/// Find the `runt` or `runt-nightly` binary on PATH or in platform-specific locations.
fn find_runt_binary(channel: &str) -> Option<PathBuf> {
    let binary_name = if channel == "nightly" {
        runt_workspace::cli_command_name_for(runt_workspace::BuildChannel::Nightly)
    } else {
        runt_workspace::cli_command_name_for(runt_workspace::BuildChannel::Stable)
    };

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

#[tokio::main]
async fn main() -> ExitCode {
    // Log to stderr (MCP uses stdout for transport)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let channel = std::env::var("NTERACT_CHANNEL").unwrap_or_else(|_| "stable".to_string());
    let app_name = if channel == "nightly" {
        "nteract Nightly"
    } else {
        "nteract"
    };

    info!("mcpb-runt starting (channel={channel})");

    // Validate that the runt binary exists at startup (clear error for users)
    let binary_name = if channel == "nightly" {
        "runt-nightly"
    } else {
        "runt"
    };
    if find_runt_binary(&channel).is_none() {
        eprintln!(
            "Error: {binary_name} not found.\n\n\
             Install {app_name} from https://nteract.io to use this MCP server.\n\
             The app puts {binary_name} on your PATH during installation."
        );
        return ExitCode::FAILURE;
    }
    info!("Validated {binary_name} is available");

    // Resolve daemon info path for version tracking
    let daemon_info_path = Some(runtimed_client::singleton::daemon_info_path());

    // Build child environment
    let mut child_env = HashMap::new();
    child_env.insert("NTERACT_CHANNEL".to_string(), channel.clone());

    // Pass resolution as a closure so the proxy re-discovers the binary
    // on every child restart. This is the core upgrade mechanism: the user
    // upgrades the nteract app (new runt binary), and the proxy picks it up
    // without needing to reinstall the MCPB extension.
    let channel_for_resolve = channel.clone();
    let binary_name_for_resolve = binary_name.to_string();
    let config = ProxyConfig {
        resolve_child_command: Box::new(move || {
            find_runt_binary(&channel_for_resolve)
                .ok_or_else(|| format!("{binary_name_for_resolve} no longer found on PATH or in known install locations"))
        }),
        child_args: vec!["mcp".to_string()],
        child_env,
        server_name: if channel == "nightly" {
            "nteract-nightly".to_string()
        } else {
            "nteract".to_string()
        },
        cache_dir: dirs::cache_dir().map(|d| {
            let namespace = if channel == "nightly" {
                "runt-nightly"
            } else {
                "runt"
            };
            let dir = d.join(namespace).join("mcpb");
            let _ = std::fs::create_dir_all(&dir);
            dir
        }),
        daemon_info_path,
    };

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

    // Background: initialize child process
    let proxy_for_init = proxy_ref.clone();
    let peer = server.peer().clone();
    tokio::spawn(async move {
        if let Err(e) = proxy_for_init.init_child().await {
            error!("Failed to initialize child: {e}");
            return;
        }

        // Notify client that tools are available
        if let Err(e) = peer.notify_tool_list_changed().await {
            tracing::warn!("Failed to send tools/list_changed: {e}");
        }
        if let Err(e) = peer.notify_resource_list_changed().await {
            tracing::warn!("Failed to send resources/list_changed: {e}");
        }
        info!("Child initialized, tools available");
    });

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
