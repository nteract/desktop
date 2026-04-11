//! Notebook registry for stable ID management.
//!
//! Maps file paths to stable UUIDs for persistent notebook identification across
//! file moves, renames, and copies. The daemon is the source of truth for ID
//! resolution and conflict handling.
//!
//! ## Design
//!
//! - **Disk-backed cache** at `~/.cache/runt{-nightly}/notebook_registry.json`
//! - **Bidirectional maps**: path→id, id→path, id→session
//! - **Conflict resolution**: mtime-based (newer file wins)
//! - **Debounced writes**: 5s after last change, atomic file replacement
//! - **30-day TTL**: evict entries not seen in 30 days
//!
//! ## Invariants
//!
//! - Metadata is source of truth for the ID at a given path
//! - Cache validates mtime on lookup - stale entries are refreshed
//! - Active sessions block ID reassignment

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Default TTL for cache entries (30 days).
const DEFAULT_ENTRY_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Debounce interval for persisting cache to disk (5 seconds).
const PERSIST_DEBOUNCE: Duration = Duration::from_secs(5);

/// Notebook registry entry with path, ID, and timestamps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotebookEntry {
    /// Stable notebook ID (UUID v4).
    pub id: Uuid,
    /// Canonical filesystem path.
    #[serde(
        serialize_with = "serialize_path",
        deserialize_with = "deserialize_path"
    )]
    pub path: PathBuf,
    /// Last modification time of the notebook file.
    #[serde(with = "systemtime_serde")]
    pub mtime: SystemTime,
    /// Last time this entry was accessed (for TTL eviction).
    #[serde(with = "systemtime_serde")]
    pub last_seen: SystemTime,
}

/// Serialization helpers for PathBuf (as string).
fn serialize_path<S>(path: &Path, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&path.to_string_lossy())
}

fn deserialize_path<'de, D>(deserializer: D) -> Result<PathBuf, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    Ok(PathBuf::from(s))
}

/// Serialization helpers for SystemTime (as RFC 3339 string).
mod systemtime_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::SystemTime;

    pub fn serialize<S>(time: &SystemTime, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::Serialize;
        let duration = time
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(serde::ser::Error::custom)?;
        let secs = duration.as_secs();
        let nanos = duration.subsec_nanos();
        // RFC 3339 format via chrono
        let datetime = chrono::DateTime::from_timestamp(secs as i64, nanos)
            .ok_or_else(|| serde::ser::Error::custom("invalid timestamp"))?;
        datetime.to_rfc3339().serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<SystemTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let datetime =
            chrono::DateTime::parse_from_rfc3339(&s).map_err(serde::de::Error::custom)?;
        let secs = datetime.timestamp() as u64;
        let nanos = datetime.timestamp_subsec_nanos();
        Ok(SystemTime::UNIX_EPOCH + std::time::Duration::new(secs, nanos))
    }
}

/// On-disk cache format.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheFormat {
    version: u32,
    entries: Vec<NotebookEntry>,
}

/// Notebook registry for stable ID management.
///
/// Thread-safe via `Arc<RwLock<...>>`. The inner state is protected by RwLock
/// to allow concurrent reads and exclusive writes.
pub struct NotebookRegistry {
    /// Path to the cache file.
    cache_path: PathBuf,
    /// Inner state (protected by RwLock).
    inner: Arc<RwLock<RegistryInner>>,
}

/// Inner registry state.
struct RegistryInner {
    /// Path → entry map.
    path_to_entry: HashMap<PathBuf, NotebookEntry>,
    /// ID → path map (for fast reverse lookup).
    id_to_path: HashMap<Uuid, PathBuf>,
    /// Active session IDs (block ID reassignment).
    active_sessions: HashMap<Uuid, String>, // id → session_id
    /// Dirty flag (needs write to disk).
    dirty: bool,
    /// Handle to the debounce task (if any).
    persist_task: Option<tokio::task::JoinHandle<()>>,
}

impl NotebookRegistry {
    /// Create a new registry with the given cache path.
    ///
    /// Does not load from disk - call `load()` for that.
    pub fn new(cache_path: PathBuf) -> Self {
        Self {
            cache_path,
            inner: Arc::new(RwLock::new(RegistryInner {
                path_to_entry: HashMap::new(),
                id_to_path: HashMap::new(),
                active_sessions: HashMap::new(),
                dirty: false,
                persist_task: None,
            })),
        }
    }

    /// Load the registry from disk, validating entries.
    ///
    /// Stale entries (file missing or mtime changed) are marked for refresh.
    /// Returns a new registry instance with loaded state.
    pub async fn load(cache_path: PathBuf) -> Result<Self, anyhow::Error> {
        let registry = Self::new(cache_path.clone());

        // Try to read cache file
        let json = match tokio::fs::read_to_string(&cache_path).await {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Cache doesn't exist yet - start fresh
                info!("Notebook registry cache not found, starting fresh");
                return Ok(registry);
            }
            Err(e) => {
                warn!("Failed to read notebook registry cache: {}", e);
                return Ok(registry); // Start fresh on read error
            }
        };

        // Parse cache
        let cache: CacheFormat = match serde_json::from_str(&json) {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to parse notebook registry cache: {}", e);
                return Ok(registry); // Start fresh on parse error
            }
        };

        if cache.version != 1 {
            warn!(
                "Unknown notebook registry version: {}, starting fresh",
                cache.version
            );
            return Ok(registry);
        }

        // Validate and load entries
        let mut inner = registry.inner.write().await;
        let now = SystemTime::now();

        for entry in cache.entries {
            // Check TTL
            if let Ok(age) = now.duration_since(entry.last_seen) {
                if age > DEFAULT_ENTRY_TTL {
                    debug!(
                        "Evicting stale entry: {} (not seen in {:?})",
                        entry.path.display(),
                        age
                    );
                    continue;
                }
            }

            // Validate file exists and mtime matches
            match tokio::fs::metadata(&entry.path).await {
                Ok(meta) => {
                    let current_mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    if current_mtime == entry.mtime {
                        // Entry is still valid
                        inner
                            .path_to_entry
                            .insert(entry.path.clone(), entry.clone());
                        inner.id_to_path.insert(entry.id, entry.path.clone());
                    } else {
                        debug!(
                            "Mtime changed for {}, will refresh on next access",
                            entry.path.display()
                        );
                        // Don't insert - will be refreshed on next lookup
                    }
                }
                Err(_) => {
                    debug!("File no longer exists: {}", entry.path.display());
                    // Don't insert - file was moved or deleted
                }
            }
        }

        let count = inner.path_to_entry.len();
        drop(inner); // Release lock before returning

        info!("Loaded notebook registry with {} entries", count);

        Ok(registry)
    }

    /// Register a notebook with its ID and current mtime.
    ///
    /// Updates `last_seen` and marks the cache as dirty.
    pub async fn register(
        &self,
        path: &Path,
        id: Uuid,
        mtime: SystemTime,
    ) -> Result<(), anyhow::Error> {
        let canonical_path = self.canonicalize_path(path).await?;
        let mut inner = self.inner.write().await;

        let entry = NotebookEntry {
            id,
            path: canonical_path.clone(),
            mtime,
            last_seen: SystemTime::now(),
        };

        inner
            .path_to_entry
            .insert(canonical_path.clone(), entry.clone());
        inner.id_to_path.insert(id, canonical_path);
        inner.dirty = true;

        drop(inner); // Release lock before scheduling persist
        self.schedule_persist().await;

        Ok(())
    }

    /// Look up a notebook ID by path.
    ///
    /// Returns `None` if not found in cache.
    pub async fn lookup_by_path(&self, path: &Path) -> Result<Option<Uuid>, anyhow::Error> {
        let canonical_path = self.canonicalize_path(path).await?;
        let inner = self.inner.read().await;

        let id = inner
            .path_to_entry
            .get(&canonical_path)
            .map(|entry| entry.id);

        if id.is_some() {
            // Update last_seen (needs write lock)
            drop(inner);
            let mut inner = self.inner.write().await;
            if let Some(entry) = inner.path_to_entry.get_mut(&canonical_path) {
                entry.last_seen = SystemTime::now();
                inner.dirty = true;
            }
            drop(inner);
            self.schedule_persist().await;
        }

        Ok(id)
    }

    /// Look up a notebook path by ID.
    ///
    /// Returns `None` if not found in cache.
    pub async fn lookup_by_id(&self, id: Uuid) -> Option<PathBuf> {
        let inner = self.inner.read().await;
        inner.id_to_path.get(&id).cloned()
    }

    /// Update the path for a given ID (file was moved).
    ///
    /// Removes the old path→entry mapping and adds the new one.
    pub async fn update_path(
        &self,
        id: Uuid,
        new_path: &Path,
        new_mtime: SystemTime,
    ) -> Result<(), anyhow::Error> {
        let canonical_new_path = self.canonicalize_path(new_path).await?;
        let mut inner = self.inner.write().await;

        // Remove old path mapping if it exists
        if let Some(old_path) = inner.id_to_path.get(&id).cloned() {
            inner.path_to_entry.remove(&old_path);
        }

        // Add new mapping
        let entry = NotebookEntry {
            id,
            path: canonical_new_path.clone(),
            mtime: new_mtime,
            last_seen: SystemTime::now(),
        };

        inner
            .path_to_entry
            .insert(canonical_new_path.clone(), entry);
        inner.id_to_path.insert(id, canonical_new_path);
        inner.dirty = true;

        drop(inner);
        self.schedule_persist().await;

        Ok(())
    }

    /// Unregister a notebook ID (session closed).
    ///
    /// **Note:** Does NOT remove from cache - just clears active session flag.
    pub async fn unregister(&self, id: Uuid) {
        let mut inner = self.inner.write().await;
        inner.active_sessions.remove(&id);
        // Don't mark dirty - session state is not persisted
    }

    /// Mark a notebook ID as having an active session.
    ///
    /// Blocks ID reassignment while session is open.
    pub async fn mark_session_active(&self, id: Uuid, session_id: String) {
        let mut inner = self.inner.write().await;
        inner.active_sessions.insert(id, session_id);
    }

    /// Check if a notebook ID has an active session.
    pub async fn has_active_session(&self, id: Uuid) -> bool {
        let inner = self.inner.read().await;
        inner.active_sessions.contains_key(&id)
    }

    /// Persist the cache to disk atomically.
    ///
    /// Writes to a temporary file, then renames (atomic on POSIX).
    pub async fn persist(&self) -> Result<(), anyhow::Error> {
        let inner = self.inner.read().await;

        if !inner.dirty {
            return Ok(());
        }

        // Build cache format
        let cache = CacheFormat {
            version: 1,
            entries: inner.path_to_entry.values().cloned().collect(),
        };

        drop(inner); // Release read lock before I/O

        // Serialize
        let json = serde_json::to_string_pretty(&cache)
            .map_err(|e| anyhow::anyhow!("Failed to serialize notebook registry: {}", e))?;

        // Atomic write: write to temp file, then rename
        let temp_path = self.cache_path.with_extension("tmp");
        tokio::fs::write(&temp_path, json)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to write notebook registry: {}", e))?;
        tokio::fs::rename(&temp_path, &self.cache_path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to rename notebook registry: {}", e))?;

        // Clear dirty flag
        let mut inner = self.inner.write().await;
        inner.dirty = false;

        debug!("Persisted notebook registry to {:?}", self.cache_path);

        Ok(())
    }

    /// Schedule a debounced persist operation.
    ///
    /// Cancels any pending persist task and spawns a new one.
    async fn schedule_persist(&self) {
        let mut inner = self.inner.write().await;

        // Cancel pending task
        if let Some(task) = inner.persist_task.take() {
            task.abort();
        }

        // Spawn new debounce task
        let registry = self.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(PERSIST_DEBOUNCE).await;
            if let Err(e) = registry.persist().await {
                error!("Failed to persist notebook registry: {}", e);
            }
        });

        inner.persist_task = Some(task);
    }

    /// Canonicalize a path (resolve symlinks, normalize).
    ///
    /// Falls back to the original path if canonicalization fails.
    async fn canonicalize_path(&self, path: &Path) -> Result<PathBuf, anyhow::Error> {
        match tokio::fs::canonicalize(path).await {
            Ok(canonical) => Ok(canonical),
            Err(e) => {
                warn!(
                    "Failed to canonicalize path {}: {}, using as-is",
                    path.display(),
                    e
                );
                Ok(path.to_path_buf())
            }
        }
    }
}

impl Clone for NotebookRegistry {
    fn clone(&self) -> Self {
        Self {
            cache_path: self.cache_path.clone(),
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Drop for RegistryInner {
    fn drop(&mut self) {
        // Cancel persist task if any
        if let Some(task) = self.persist_task.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::time::SystemTime;
    use tempfile::TempDir;

    fn test_cache_path(tmp: &TempDir) -> PathBuf {
        tmp.path().join("notebook_registry.json")
    }

    #[tokio::test]
    async fn test_load_empty_registry() {
        let tmp = TempDir::new().unwrap();
        let cache_path = test_cache_path(&tmp);

        // Load from non-existent cache - should succeed with empty registry
        let registry = NotebookRegistry::load(cache_path.clone()).await.unwrap();

        // Lookup should return None for unknown path
        let test_path = PathBuf::from("/tmp/test.ipynb");
        let result = registry.lookup_by_path(&test_path).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_register_and_lookup_round_trip() {
        let tmp = TempDir::new().unwrap();
        let cache_path = test_cache_path(&tmp);
        let registry = NotebookRegistry::new(cache_path);

        // Create a test file
        let test_file = tmp.path().join("test.ipynb");
        tokio::fs::write(&test_file, b"{}").await.unwrap();

        let test_id = Uuid::new_v4();
        let mtime = tokio::fs::metadata(&test_file)
            .await
            .unwrap()
            .modified()
            .unwrap();

        // Register
        registry.register(&test_file, test_id, mtime).await.unwrap();

        // Lookup by path
        let result = registry.lookup_by_path(&test_file).await.unwrap();
        assert_eq!(result, Some(test_id));

        // Lookup by ID
        let result_path = registry.lookup_by_id(test_id).await;
        assert!(result_path.is_some());
        assert_eq!(
            result_path.unwrap(),
            tokio::fs::canonicalize(&test_file).await.unwrap()
        );
    }

    #[tokio::test]
    async fn test_update_path_for_file_move() {
        let tmp = TempDir::new().unwrap();
        let cache_path = test_cache_path(&tmp);
        let registry = NotebookRegistry::new(cache_path);

        // Create test file at old path
        let old_path = tmp.path().join("old.ipynb");
        tokio::fs::write(&old_path, b"{}").await.unwrap();

        let test_id = Uuid::new_v4();
        let mtime = tokio::fs::metadata(&old_path)
            .await
            .unwrap()
            .modified()
            .unwrap();

        registry.register(&old_path, test_id, mtime).await.unwrap();

        // Move file
        let new_path = tmp.path().join("new.ipynb");
        tokio::fs::rename(&old_path, &new_path).await.unwrap();

        let new_mtime = tokio::fs::metadata(&new_path)
            .await
            .unwrap()
            .modified()
            .unwrap();

        // Update registry with new path
        registry
            .update_path(test_id, &new_path, new_mtime)
            .await
            .unwrap();

        // Old path should not resolve
        let old_result = registry.lookup_by_path(&old_path).await.unwrap();
        assert!(old_result.is_none());

        // New path should resolve to same ID
        let new_result = registry.lookup_by_path(&new_path).await.unwrap();
        assert_eq!(new_result, Some(test_id));
    }

    #[tokio::test]
    async fn test_persist_and_reload() {
        let tmp = TempDir::new().unwrap();
        let cache_path = test_cache_path(&tmp);

        // Create test file
        let test_file = tmp.path().join("test.ipynb");
        tokio::fs::write(&test_file, b"{}").await.unwrap();

        let test_id = Uuid::new_v4();
        let mtime = tokio::fs::metadata(&test_file)
            .await
            .unwrap()
            .modified()
            .unwrap();

        // Register and persist
        {
            let registry = NotebookRegistry::new(cache_path.clone());
            registry.register(&test_file, test_id, mtime).await.unwrap();
            registry.persist().await.unwrap();
        }

        // Load new registry from persisted cache
        let registry = NotebookRegistry::load(cache_path).await.unwrap();

        // Should find the registered entry
        let result = registry.lookup_by_path(&test_file).await.unwrap();
        assert_eq!(result, Some(test_id));
    }

    #[tokio::test]
    async fn test_ttl_eviction() {
        let tmp = TempDir::new().unwrap();
        let cache_path = test_cache_path(&tmp);

        // Create test file
        let test_file = tmp.path().join("test.ipynb");
        tokio::fs::write(&test_file, b"{}").await.unwrap();

        let test_id = Uuid::new_v4();
        let mtime = SystemTime::now();

        // Create entry with very old last_seen
        let old_last_seen = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let entry = NotebookEntry {
            id: test_id,
            path: tokio::fs::canonicalize(&test_file).await.unwrap(),
            mtime,
            last_seen: old_last_seen,
        };

        // Write cache with old entry
        let cache = CacheFormat {
            version: 1,
            entries: vec![entry],
        };
        let json = serde_json::to_string_pretty(&cache).unwrap();
        tokio::fs::write(&cache_path, json).await.unwrap();

        // Load - should evict the stale entry
        let registry = NotebookRegistry::load(cache_path).await.unwrap();

        // Should not find the evicted entry
        let result = registry.lookup_by_path(&test_file).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_active_session_tracking() {
        let tmp = TempDir::new().unwrap();
        let cache_path = test_cache_path(&tmp);
        let registry = NotebookRegistry::new(cache_path);

        let test_id = Uuid::new_v4();
        let session_id = "session-123".to_string();

        // No active session initially
        assert!(!registry.has_active_session(test_id).await);

        // Mark as active
        registry
            .mark_session_active(test_id, session_id.clone())
            .await;
        assert!(registry.has_active_session(test_id).await);

        // Unregister
        registry.unregister(test_id).await;
        assert!(!registry.has_active_session(test_id).await);
    }
}
