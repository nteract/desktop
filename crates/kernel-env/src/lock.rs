//! Lock file persistence for conda/pixi environments.
//!
//! After a successful solve + install, we write a `.lock.json` file alongside
//! the environment prefix. On subsequent rebuilds (e.g. corrupted env where
//! the directory exists but python is missing), we can skip the repodata fetch
//! and solve phases entirely and install directly from the lock.
//!
//! All lock operations are non-fatal — failures log warnings and fall back to
//! the normal solve path.

use anyhow::Result;
use log::{info, warn};
use rattler_conda_types::{Platform, RepoDataRecord};
use serde::{Deserialize, Serialize};
use std::path::Path;

const LOCK_FILE_NAME: &str = ".lock.json";

/// Persisted solve result for a conda/pixi environment.
///
/// Contains everything needed to recreate the environment without
/// re-fetching repodata or re-solving: the original specs, channels,
/// platform, and the full list of resolved packages.
#[derive(Debug, Serialize, Deserialize)]
pub struct LockFile {
    /// Schema version (currently 1).
    pub version: u32,
    /// Platform the solve was performed on (e.g. `osx-arm64`).
    pub platform: String,
    /// Original dependency specs (e.g. `["python>=3.13", "numpy"]`).
    pub specs: Vec<String>,
    /// Channels used for the solve (e.g. `["conda-forge"]`).
    pub channels: Vec<String>,
    /// Fully resolved packages from the solver.
    pub packages: Vec<RepoDataRecord>,
}

impl LockFile {
    /// Create a new lock file from solve results.
    pub fn new(specs: Vec<String>, channels: Vec<String>, packages: Vec<RepoDataRecord>) -> Self {
        Self {
            version: 1,
            platform: Platform::current().to_string(),
            specs,
            channels,
            packages,
        }
    }

    /// Write the lock file to `<env_path>/.lock.json`.
    pub async fn write_to(&self, env_path: &Path) -> Result<()> {
        let lock_path = env_path.join(LOCK_FILE_NAME);
        let json = serde_json::to_string(self)?;
        tokio::fs::write(&lock_path, json).await?;
        info!("Wrote lock file to {:?}", lock_path);
        Ok(())
    }

    /// Read a lock file from `<env_path>/.lock.json`, returning `None` if
    /// missing or unparseable.
    pub async fn read_from(env_path: &Path) -> Option<Self> {
        let lock_path = env_path.join(LOCK_FILE_NAME);
        let data = tokio::fs::read_to_string(&lock_path).await.ok()?;
        match serde_json::from_str(&data) {
            Ok(lock) => Some(lock),
            Err(e) => {
                warn!("Failed to parse lock file at {:?}: {}", lock_path, e);
                None
            }
        }
    }

    /// Check if this lock matches the given specs and channels (order-independent).
    pub fn matches(&self, specs: &[String], channels: &[String]) -> bool {
        self.version == 1
            && self.platform == Platform::current().to_string()
            && sorted_eq(&self.specs, specs)
            && sorted_eq(&self.channels, channels)
    }
}

fn sorted_eq(a: &[String], b: &[String]) -> bool {
    let mut a: Vec<_> = a.iter().map(|s| s.to_lowercase()).collect();
    let mut b: Vec<_> = b.iter().map(|s| s.to_lowercase()).collect();
    a.sort();
    b.sort();
    a == b
}

/// Non-fatal lock file write. Logs warning on failure.
pub async fn try_write_lock(env_path: &Path, lock: &LockFile) {
    if let Err(e) = lock.write_to(env_path).await {
        warn!("Failed to write lock file: {}", e);
    }
}

/// Install packages from a lock file directly, skipping repodata + solve.
pub async fn install_from_lock(
    env_path: &Path,
    lock: &LockFile,
    handler: std::sync::Arc<dyn crate::progress::ProgressHandler>,
    env_type: &str,
) -> Result<()> {
    use rattler::install::Installer;
    use rattler_conda_types::Platform;

    handler.on_progress(env_type, crate::progress::EnvProgressPhase::LockFileHit);

    let download_client = reqwest::Client::builder().build()?;
    let download_client = reqwest_middleware::ClientBuilder::new(download_client).build();

    let install_platform = Platform::current();

    handler.on_progress(
        env_type,
        crate::progress::EnvProgressPhase::Installing {
            total: lock.packages.len(),
        },
    );

    let reporter = crate::progress::RattlerReporter::new_with_env_type(handler.clone(), env_type);

    Installer::new()
        .with_download_client(download_client)
        .with_target_platform(install_platform)
        .with_reporter(reporter)
        .install(env_path, lock.packages.clone())
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lock_file_matches() {
        let lock = LockFile::new(
            vec!["python>=3.13".into(), "numpy".into()],
            vec!["conda-forge".into()],
            vec![],
        );
        assert!(lock.matches(
            &["numpy".into(), "python>=3.13".into()],
            &["conda-forge".into()]
        ));
    }

    #[test]
    fn test_lock_file_no_match_different_specs() {
        let lock = LockFile::new(vec!["numpy".into()], vec!["conda-forge".into()], vec![]);
        assert!(!lock.matches(&["pandas".into()], &["conda-forge".into()]));
    }

    #[test]
    fn test_lock_file_no_match_different_channels() {
        let lock = LockFile::new(vec!["numpy".into()], vec!["conda-forge".into()], vec![]);
        assert!(!lock.matches(&["numpy".into()], &["defaults".into()]));
    }

    #[tokio::test]
    async fn test_lock_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let lock = LockFile::new(
            vec!["numpy".into(), "pandas".into()],
            vec!["conda-forge".into()],
            vec![],
        );
        lock.write_to(dir.path()).await.unwrap();
        let loaded = LockFile::read_from(dir.path()).await.unwrap();
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.specs, lock.specs);
        assert_eq!(loaded.channels, lock.channels);
    }

    #[tokio::test]
    async fn test_lock_file_read_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(LockFile::read_from(dir.path()).await.is_none());
    }
}
