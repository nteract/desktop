//! Garbage collection for content-addressed environment caches.
//!
//! Cached environments (UV, Conda, inline) are stored as `{16-char-hash}/`
//! directories. This module provides:
//!
//! - A `.last-used` timestamp file to track when an env was last accessed
//! - An eviction function that removes envs older than `max_age` or beyond
//!   `max_count`, keeping the most recently used ones

use anyhow::Result;
use log::{info, warn};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Touch (create or update) the `.last-used` marker in an environment directory.
///
/// Call this on cache hit and on fresh creation so the GC knows when the env
/// was last accessed.
pub async fn touch_last_used(env_path: &Path) {
    let marker = env_path.join(".last-used");
    // Write current unix timestamp as text for easy debugging
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();
    if let Err(e) = tokio::fs::write(&marker, now.as_bytes()).await {
        warn!("Failed to update .last-used in {:?}: {}", env_path, e);
    }
}

/// Read the last-used time for an environment directory.
///
/// Checks `.last-used` marker first, falls back to directory mtime.
async fn last_used_time(env_path: &Path) -> SystemTime {
    let marker = env_path.join(".last-used");
    if let Ok(contents) = tokio::fs::read_to_string(&marker).await {
        if let Ok(secs) = contents.trim().parse::<u64>() {
            return SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        }
    }
    // Fall back to directory mtime
    tokio::fs::metadata(env_path)
        .await
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Returns true if `name` looks like a 16-char hex content-addressed hash.
fn is_content_addressed_dir(name: &str) -> bool {
    name.len() == 16 && name.chars().all(|c| c.is_ascii_hexdigit())
}

/// Evict stale content-addressed environments from a cache directory.
///
/// Scans `cache_dir` for directories matching the 16-char hex pattern.
/// Deletes those older than `max_age` or beyond `max_count` (keeping newest).
/// Skips `runtimed-uv-*` and `runtimed-conda-*` dirs (pool-managed).
///
/// Returns the list of deleted directory paths.
pub async fn evict_stale_envs(
    cache_dir: &Path,
    max_age: Duration,
    max_count: usize,
) -> Result<Vec<PathBuf>> {
    let mut deleted = Vec::new();

    if !cache_dir.exists() {
        return Ok(deleted);
    }

    let mut entries = tokio::fs::read_dir(cache_dir).await?;
    let mut candidates: Vec<(PathBuf, SystemTime)> = Vec::new();

    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();

        // Only process content-addressed dirs (16-char hex names)
        if !is_content_addressed_dir(&name) {
            continue;
        }

        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let last_used = last_used_time(&path).await;
        candidates.push((path, last_used));
    }

    // Sort by last-used time descending (newest first)
    candidates.sort_by(|a, b| b.1.cmp(&a.1));

    let now = SystemTime::now();

    for (i, (path, last_used)) in candidates.iter().enumerate() {
        let age = now.duration_since(*last_used).unwrap_or_default();
        let beyond_count = i >= max_count;
        let beyond_age = age > max_age;

        if beyond_count || beyond_age {
            info!(
                "[gc] Evicting cached env {:?} (age: {}h, index: {})",
                path,
                age.as_secs() / 3600,
                i
            );
            if let Err(e) = tokio::fs::remove_dir_all(&path).await {
                warn!("[gc] Failed to remove {:?}: {}", path, e);
            } else {
                deleted.push(path.clone());
            }
        }
    }

    if !deleted.is_empty() {
        info!(
            "[gc] Evicted {} cached environments from {:?}",
            deleted.len(),
            cache_dir
        );
    }

    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_is_content_addressed_dir() {
        assert!(is_content_addressed_dir("52cc822d70a6d5d7"));
        assert!(is_content_addressed_dir("e3b0c44298fc1c14"));
        assert!(!is_content_addressed_dir("runtimed-uv-abc"));
        assert!(!is_content_addressed_dir("short"));
        assert!(!is_content_addressed_dir("52cc822d70a6d5d7extra"));
        assert!(!is_content_addressed_dir("zzzzzzzzzzzzzzzz"));
    }

    #[tokio::test]
    async fn test_touch_and_read_last_used() {
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join("abcdef0123456789");
        tokio::fs::create_dir_all(&env_path).await.unwrap();

        touch_last_used(&env_path).await;

        let marker = env_path.join(".last-used");
        assert!(marker.exists());

        let contents = tokio::fs::read_to_string(&marker).await.unwrap();
        let ts: u64 = contents.trim().parse().unwrap();
        // Should be within the last few seconds
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(now - ts < 5);
    }

    #[tokio::test]
    async fn test_evict_by_count() {
        let tmp = TempDir::new().unwrap();

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Create 5 content-addressed dirs with staggered recent timestamps
        for i in 0..5u64 {
            let name = format!("{:016x}", i);
            let dir = tmp.path().join(&name);
            tokio::fs::create_dir_all(&dir).await.unwrap();
            // Stagger by 1 hour, all recent
            let ts = now - (4 - i) * 3600;
            tokio::fs::write(dir.join(".last-used"), ts.to_string())
                .await
                .unwrap();
        }

        // Also create a pool dir that should be skipped
        let pool_dir = tmp.path().join("runtimed-uv-test");
        tokio::fs::create_dir_all(&pool_dir).await.unwrap();

        // Evict to max_count=2
        let deleted = evict_stale_envs(tmp.path(), Duration::from_secs(86400 * 365), 2)
            .await
            .unwrap();

        // Should have deleted 3 (the 3 oldest)
        assert_eq!(deleted.len(), 3);

        // Pool dir should still exist
        assert!(pool_dir.exists());

        // The 2 newest should remain
        assert!(tmp.path().join(format!("{:016x}", 4)).exists());
        assert!(tmp.path().join(format!("{:016x}", 3)).exists());
    }

    #[tokio::test]
    async fn test_evict_by_age() {
        let tmp = TempDir::new().unwrap();

        // Create a dir with a very old timestamp
        let old_dir = tmp.path().join("aaaaaaaaaaaaaaaa");
        tokio::fs::create_dir_all(&old_dir).await.unwrap();
        tokio::fs::write(old_dir.join(".last-used"), "1000000")
            .await
            .unwrap();

        // Create a dir with a recent timestamp
        let new_dir = tmp.path().join("bbbbbbbbbbbbbbbb");
        tokio::fs::create_dir_all(&new_dir).await.unwrap();
        touch_last_used(&new_dir).await;

        // Evict with max_age=1 day, high max_count
        let deleted = evict_stale_envs(tmp.path(), Duration::from_secs(86400), 100)
            .await
            .unwrap();

        assert_eq!(deleted.len(), 1);
        assert!(!old_dir.exists());
        assert!(new_dir.exists());
    }
}
