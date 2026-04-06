//! Content-addressed blob store for notebook outputs.
//!
//! Stores blobs (images, HTML, rich data) on disk at a configurable root
//! directory. Each blob is identified by its SHA-256 hash (hex-encoded) and
//! stored in a two-level shard directory:
//!
//! ```text
//! <root>/
//!   a1/
//!     b2c3d4...       # raw bytes
//!     b2c3d4....meta  # JSON metadata sidecar
//! ```
//!
//! All writes are atomic: data is written to a temp file in the shard
//! directory and renamed into place, so readers never see partial writes.

use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Maximum blob size accepted by `put()` (100 MiB).
pub const MAX_BLOB_SIZE: usize = 100 * 1024 * 1024;

/// Metadata stored alongside each blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobMeta {
    pub media_type: String,
    pub size: u64,
    pub created_at: DateTime<Utc>,
}

/// Content-addressed on-disk blob store.
#[derive(Debug, Clone)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    /// Create a new BlobStore rooted at `root`.
    ///
    /// The directory is created lazily on first `put()`.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Get the root directory of this blob store.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Store `data` with the given `media_type`.
    ///
    /// Returns the SHA-256 hex hash of the raw bytes.
    /// Rejects data larger than 100 MiB.
    /// Idempotent: if the blob already exists, returns the hash without writing.
    ///
    /// Concurrent puts of identical content are safe: if another writer places
    /// the blob or metadata first (e.g. `rename` fails with `AlreadyExists` on
    /// Windows), we detect the existing file and return `Ok(hash)`.
    pub async fn put(&self, data: &[u8], media_type: &str) -> io::Result<String> {
        if data.len() > MAX_BLOB_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "blob too large: {} bytes (max {})",
                    data.len(),
                    MAX_BLOB_SIZE
                ),
            ));
        }

        let hash = hex::encode(Sha256::digest(data));
        let (shard_dir, blob_path, meta_path) = self.paths(&hash);

        // Fast path: both files already present.
        // If the caller provides a different media_type than what's stored,
        // update the metadata sidecar (e.g., blob was first stored as
        // application/json but is now text/javascript for anywidget _esm).
        if blob_path.exists() && meta_path.exists() {
            if let Ok(existing_meta_json) = tokio::fs::read_to_string(&meta_path).await {
                if let Ok(existing_meta) = serde_json::from_str::<BlobMeta>(&existing_meta_json) {
                    if existing_meta.media_type != media_type {
                        let updated = BlobMeta {
                            media_type: media_type.to_string(),
                            ..existing_meta
                        };
                        if let Ok(json) = serde_json::to_string(&updated) {
                            tokio::fs::write(&meta_path, json).await.ok();
                        }
                    }
                }
            }
            return Ok(hash);
        }

        tokio::fs::create_dir_all(&shard_dir).await?;

        // --- Blob ---
        // Write to a temp file and atomically rename into place.
        // On Windows `rename` fails with AlreadyExists when the target exists,
        // so a concurrent put of the same content can race here. Since the hash
        // is derived from the bytes, any existing file with the same name has
        // identical content — we just need to ensure the metadata sidecar exists.
        let we_wrote_blob;
        let tmp_blob = shard_dir.join(format!(".tmp.{}", uuid::Uuid::new_v4()));
        match async {
            tokio::fs::write(&tmp_blob, data).await?;
            tokio::fs::rename(&tmp_blob, &blob_path).await
        }
        .await
        {
            Ok(()) => {
                we_wrote_blob = true;
            }
            Err(e) => {
                tokio::fs::remove_file(&tmp_blob).await.ok();
                if blob_path.exists() {
                    // Concurrent writer placed the blob — proceed to metadata.
                    we_wrote_blob = false;
                } else {
                    return Err(e);
                }
            }
        }

        // --- Metadata sidecar ---
        let meta = BlobMeta {
            media_type: media_type.to_string(),
            size: data.len() as u64,
            created_at: Utc::now(),
        };
        let meta_json = serde_json::to_string(&meta).map_err(io::Error::other)?;

        let tmp_meta = shard_dir.join(format!(".tmp.{}.meta", uuid::Uuid::new_v4()));
        match async {
            tokio::fs::write(&tmp_meta, meta_json).await?;
            tokio::fs::rename(&tmp_meta, &meta_path).await
        }
        .await
        {
            Ok(()) => {}
            Err(e) => {
                tokio::fs::remove_file(&tmp_meta).await.ok();
                if meta_path.exists() {
                    // Concurrent writer placed metadata — done.
                    return Ok(hash);
                }
                // Metadata write truly failed. If *we* created the blob (not a
                // concurrent writer), remove it to avoid leaving orphaned data.
                if we_wrote_blob {
                    tokio::fs::remove_file(&blob_path).await.ok();
                }
                return Err(e);
            }
        }

        Ok(hash)
    }

    /// Retrieve blob bytes by hash. Returns `None` if not found.
    pub async fn get(&self, hash: &str) -> io::Result<Option<Vec<u8>>> {
        if !Self::validate_hash(hash) {
            return Ok(None);
        }
        let (_, blob_path, _) = self.paths(hash);
        match tokio::fs::read(&blob_path).await {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Retrieve blob metadata by hash. Returns `None` if not found.
    pub async fn get_meta(&self, hash: &str) -> io::Result<Option<BlobMeta>> {
        if !Self::validate_hash(hash) {
            return Ok(None);
        }
        let (_, _, meta_path) = self.paths(hash);
        match tokio::fs::read_to_string(&meta_path).await {
            Ok(json) => {
                let meta: BlobMeta = serde_json::from_str(&json)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                Ok(Some(meta))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Check if a blob exists (without reading it).
    pub fn exists(&self, hash: &str) -> bool {
        if !Self::validate_hash(hash) {
            return false;
        }
        let (_, blob_path, _) = self.paths(hash);
        blob_path.exists()
    }

    /// Delete a blob and its metadata. Returns `true` if the blob existed.
    pub async fn delete(&self, hash: &str) -> io::Result<bool> {
        if !Self::validate_hash(hash) {
            return Ok(false);
        }
        let (_, blob_path, meta_path) = self.paths(hash);
        let existed = blob_path.exists();
        if existed {
            tokio::fs::remove_file(&blob_path).await.ok();
            tokio::fs::remove_file(&meta_path).await.ok();
        }
        Ok(existed)
    }

    /// List all blob hashes in the store.
    pub async fn list(&self) -> io::Result<Vec<String>> {
        let mut hashes = Vec::new();

        if !self.root.exists() {
            return Ok(hashes);
        }

        let mut shard_entries = tokio::fs::read_dir(&self.root).await?;
        while let Some(shard) = shard_entries.next_entry().await? {
            if !shard.path().is_dir() {
                continue;
            }
            let shard_name = shard.file_name().to_string_lossy().to_string();
            if shard_name.len() != 2 || !shard_name.chars().all(|c| c.is_ascii_hexdigit()) {
                continue;
            }

            let mut blob_entries = match tokio::fs::read_dir(shard.path()).await {
                Ok(e) => e,
                Err(_) => continue,
            };
            while let Some(entry) = blob_entries.next_entry().await? {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".meta") || name.starts_with(".tmp") {
                    continue;
                }
                let full_hash = format!("{}{}", shard_name, name);
                if Self::validate_hash(&full_hash) {
                    hashes.push(full_hash);
                }
            }
        }

        Ok(hashes)
    }

    /// Compute shard dir, blob path, and meta path for a given hash.
    fn paths(&self, hash: &str) -> (PathBuf, PathBuf, PathBuf) {
        let shard = &hash[..2];
        let rest = &hash[2..];
        let shard_dir = self.root.join(shard);
        let blob_path = shard_dir.join(rest);
        let meta_path = shard_dir.join(format!("{}.meta", rest));
        (shard_dir, blob_path, meta_path)
    }

    /// Validate that a hash looks like a 64-character hex string.
    fn validate_hash(hash: &str) -> bool {
        hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_store(dir: &TempDir) -> BlobStore {
        BlobStore::new(dir.path().join("blobs"))
    }

    #[tokio::test]
    async fn test_put_and_get() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let data = b"hello world";
        let hash = store.put(data, "text/plain").await.unwrap();
        assert_eq!(hash.len(), 64);

        let retrieved = store.get(&hash).await.unwrap().unwrap();
        assert_eq!(retrieved, data);
    }

    #[tokio::test]
    async fn test_idempotent_put() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let data = b"same content";
        let hash1 = store.put(data, "text/plain").await.unwrap();
        let hash2 = store.put(data, "text/plain").await.unwrap();
        assert_eq!(hash1, hash2);
    }

    #[tokio::test]
    async fn test_same_bytes_different_media_type() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let data = b"same bytes";
        let hash1 = store.put(data, "text/plain").await.unwrap();
        let hash2 = store.put(data, "application/octet-stream").await.unwrap();
        // Same bytes = same hash (media type doesn't affect hash)
        assert_eq!(hash1, hash2);
        // Metadata updates to the latest media type (e.g., content first
        // stored as application/json then re-stored as text/javascript
        // for anywidget _esm).
        let meta = store.get_meta(&hash1).await.unwrap().unwrap();
        assert_eq!(meta.media_type, "application/octet-stream");
    }

    #[tokio::test]
    async fn test_size_limit() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let data = vec![0u8; MAX_BLOB_SIZE + 1];
        let result = store.put(&data, "application/octet-stream").await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn test_get_not_found() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let fake_hash = "a".repeat(64);
        let result = store.get(&fake_hash).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_get_meta() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let data = b"png bytes";
        let hash = store.put(data, "image/png").await.unwrap();

        let meta = store.get_meta(&hash).await.unwrap().unwrap();
        assert_eq!(meta.media_type, "image/png");
        assert_eq!(meta.size, data.len() as u64);
    }

    #[tokio::test]
    async fn test_exists() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let fake_hash = "b".repeat(64);
        assert!(!store.exists(&fake_hash));

        let hash = store.put(b"data", "text/plain").await.unwrap();
        assert!(store.exists(&hash));
    }

    #[tokio::test]
    async fn test_delete() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let hash = store.put(b"to delete", "text/plain").await.unwrap();
        assert!(store.exists(&hash));

        let deleted = store.delete(&hash).await.unwrap();
        assert!(deleted);
        assert!(!store.exists(&hash));
        assert!(store.get(&hash).await.unwrap().is_none());
        assert!(store.get_meta(&hash).await.unwrap().is_none());

        // Deleting again returns false
        let deleted_again = store.delete(&hash).await.unwrap();
        assert!(!deleted_again);
    }

    #[tokio::test]
    async fn test_list() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let hash1 = store.put(b"one", "text/plain").await.unwrap();
        let hash2 = store.put(b"two", "text/plain").await.unwrap();
        let hash3 = store.put(b"three", "text/plain").await.unwrap();

        let mut hashes = store.list().await.unwrap();
        hashes.sort();

        let mut expected = vec![hash1, hash2, hash3];
        expected.sort();

        assert_eq!(hashes, expected);
    }

    #[tokio::test]
    async fn test_list_empty_store() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let hashes = store.list().await.unwrap();
        assert!(hashes.is_empty());
    }

    #[tokio::test]
    async fn test_invalid_hash_returns_none() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        // Too short
        assert!(store.get("abc").await.unwrap().is_none());
        assert!(store.get_meta("abc").await.unwrap().is_none());
        assert!(!store.exists("abc"));
        assert!(!store.delete("abc").await.unwrap());

        // Non-hex characters
        let bad = format!("{}z", "a".repeat(63));
        assert!(store.get(&bad).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_concurrent_puts_same_content() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let data = b"concurrent content";
        let store1 = store.clone();
        let store2 = store.clone();

        let (hash1, hash2) = tokio::join!(
            async { store1.put(data, "text/plain").await.unwrap() },
            async { store2.put(data, "text/plain").await.unwrap() },
        );

        assert_eq!(hash1, hash2);
        assert_eq!(store.get(&hash1).await.unwrap().unwrap(), data);
    }
}
