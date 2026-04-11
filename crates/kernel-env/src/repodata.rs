//! Shared repodata fetching with offline-first capability.
//!
//! Provides `query_repodata_offline_first()` for Conda and Pixi environments.
//! Tries local cache first (`ForceCacheOnly`), falls back to network on miss.

#[cfg(feature = "runtime")]
use std::{path::Path, sync::Arc, time::Instant};

#[cfg(feature = "runtime")]
use anyhow::{anyhow, Result};
#[cfg(feature = "runtime")]
use log::info;
#[cfg(feature = "runtime")]
use rattler_conda_types::{Channel, MatchSpec, Platform};
#[cfg(feature = "runtime")]
use rattler_repodata_gateway::{fetch::CacheAction, ChannelConfig, Gateway, SourceConfig};

#[cfg(feature = "runtime")]
use crate::progress::{EnvProgressPhase, ProgressHandler};

/// Query repodata with offline-first strategy.
///
/// 1. Try with `CacheAction::ForceCacheOnly` (no network)
/// 2. On success, emit `OfflineHit` and return cached repodata
/// 3. On cache miss, build normal Gateway with `CacheOrFetch` + retry loop
///
/// # Arguments
/// - `channels`: Conda channels to query
/// - `platforms`: Target platforms (usually current + NoArch)
/// - `specs`: Package specs to resolve
/// - `rattler_cache_dir`: Base rattler cache directory
/// - `download_client`: HTTP client for network fetches
/// - `handler`: Progress handler for events
/// - `env_type`: Label for progress events ("conda" or "pixi")
#[cfg(feature = "runtime")]
pub async fn query_repodata_offline_first(
    channels: Vec<Channel>,
    platforms: Vec<Platform>,
    specs: Vec<MatchSpec>,
    rattler_cache_dir: &Path,
    download_client: reqwest_middleware::ClientWithMiddleware,
    handler: Arc<dyn ProgressHandler>,
    env_type: &str,
) -> Result<Vec<rattler_repodata_gateway::RepoData>> {
    use rattler::package_cache::PackageCache;

    // Try offline first: build Gateway with ForceCacheOnly
    let offline_channel_config = ChannelConfig {
        default: SourceConfig {
            cache_action: CacheAction::ForceCacheOnly,
            ..Default::default()
        },
        ..Default::default()
    };

    let offline_gateway = Gateway::builder()
        .with_cache_dir(rattler_cache_dir.join(rattler_cache::REPODATA_CACHE_DIR))
        .with_package_cache(PackageCache::new(
            rattler_cache_dir.join(rattler_cache::PACKAGE_CACHE_DIR),
        ))
        .with_client(download_client.clone())
        .with_channel_config(offline_channel_config)
        .finish();

    match offline_gateway
        .query(channels.clone(), platforms.clone(), specs.clone())
        .recursive(true)
        .await
    {
        Ok(repo_data) => {
            info!(
                "[{}] Resolved repodata from local cache (offline mode)",
                env_type
            );
            handler.on_progress(env_type, EnvProgressPhase::OfflineHit);
            return Ok(repo_data);
        }
        Err(e) => {
            info!(
                "[{}] Offline repodata query failed (expected if not cached): {}",
                env_type, e
            );
        }
    }

    // Offline failed, fall back to network with retry loop
    let gateway = Gateway::builder()
        .with_cache_dir(rattler_cache_dir.join(rattler_cache::REPODATA_CACHE_DIR))
        .with_package_cache(PackageCache::new(
            rattler_cache_dir.join(rattler_cache::PACKAGE_CACHE_DIR),
        ))
        .with_client(download_client.clone())
        .finish();

    // Query repodata with retry
    const MAX_RETRIES: u32 = 3;
    const INITIAL_DELAY_MS: u64 = 1000;

    handler.on_progress(
        env_type,
        EnvProgressPhase::FetchingRepodata {
            channels: channels.iter().map(|c| c.name().to_string()).collect(),
        },
    );

    let repodata_start = Instant::now();
    let mut last_error = None;
    let mut repo_data = None;

    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            let delay_ms = INITIAL_DELAY_MS * (1 << (attempt - 1));
            info!(
                "[{}] Retrying repodata fetch (attempt {}/{}) after {}ms...",
                env_type,
                attempt + 1,
                MAX_RETRIES,
                delay_ms
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
        }

        match gateway
            .query(channels.clone(), platforms.clone(), specs.clone())
            .recursive(true)
            .await
        {
            Ok(data) => {
                repo_data = Some(data);
                break;
            }
            Err(e) => {
                let error_str = e.to_string();
                let is_retryable = error_str.contains("500")
                    || error_str.contains("502")
                    || error_str.contains("503")
                    || error_str.contains("504")
                    || error_str.contains("timeout")
                    || error_str.contains("connection");

                if is_retryable && attempt < MAX_RETRIES - 1 {
                    info!(
                        "[{}] Transient error fetching repodata (attempt {}): {}",
                        env_type,
                        attempt + 1,
                        error_str
                    );
                    last_error = Some(e);
                    continue;
                }
                let error_msg = format!("Failed to fetch package metadata: {}", e);
                handler.on_progress(
                    env_type,
                    EnvProgressPhase::Error {
                        message: error_msg.clone(),
                    },
                );
                return Err(anyhow!(error_msg));
            }
        }
    }

    let repo_data = match repo_data {
        Some(data) => data,
        None => {
            let error_msg = format!(
                "Failed to fetch package metadata after {} retries: {}",
                MAX_RETRIES,
                last_error
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "unknown error".to_string())
            );
            handler.on_progress(
                env_type,
                EnvProgressPhase::Error {
                    message: error_msg.clone(),
                },
            );
            return Err(anyhow!(error_msg));
        }
    };

    let elapsed = repodata_start.elapsed();
    let total_records: usize = repo_data.iter().map(|r| r.len()).sum();
    handler.on_progress(
        env_type,
        EnvProgressPhase::RepodataComplete {
            record_count: total_records,
            elapsed_ms: elapsed.as_millis() as u64,
        },
    );

    Ok(repo_data)
}
