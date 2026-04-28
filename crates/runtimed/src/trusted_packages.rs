use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PackageIdentity {
    pub ecosystem: &'static str,
    pub raw_spec: String,
    pub normalized_name: String,
}

#[derive(Debug)]
enum StoreInner {
    Sqlite { conn: Mutex<Connection> },
    Unavailable { reason: String },
}

#[derive(Debug, Clone)]
pub(crate) struct TrustedPackageStore {
    inner: Arc<StoreInner>,
}

impl TrustedPackageStore {
    pub(crate) fn open(path: PathBuf) -> Result<Self> {
        ensure_private_store_file(&path)?;
        let conn = Connection::open(&path)
            .with_context(|| format!("open trusted package store {}", path.display()))?;
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS trusted_packages (
                ecosystem TEXT NOT NULL,
                normalized_name TEXT NOT NULL,
                approved_at TEXT NOT NULL,
                source TEXT NOT NULL,
                PRIMARY KEY (ecosystem, normalized_name)
            );
            "#,
        )
        .with_context(|| format!("initialize trusted package store {}", path.display()))?;

        Ok(Self {
            inner: Arc::new(StoreInner::Sqlite {
                conn: Mutex::new(conn),
            }),
        })
    }

    pub(crate) fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(StoreInner::Unavailable {
                reason: reason.into(),
            }),
        }
    }

    pub(crate) fn unavailable_reason(&self) -> Option<&str> {
        match self.inner.as_ref() {
            StoreInner::Sqlite { .. } => None,
            StoreInner::Unavailable { reason } => Some(reason.as_str()),
        }
    }

    pub(crate) fn add_from_info(&self, info: &runt_trust::TrustInfo, source: &str) -> Result<()> {
        let identities = identities_from_trust_info(info);
        if identities.is_empty() {
            return Ok(());
        }

        let StoreInner::Sqlite { conn } = self.inner.as_ref() else {
            return Ok(());
        };

        let approved_at = chrono::Utc::now().to_rfc3339();
        let mut conn = conn
            .lock()
            .map_err(|_| anyhow::anyhow!("trusted package store mutex poisoned"))?;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                r#"
                INSERT INTO trusted_packages (ecosystem, normalized_name, approved_at, source)
                VALUES (?1, ?2, ?3, ?4)
                ON CONFLICT(ecosystem, normalized_name) DO UPDATE SET
                    approved_at = excluded.approved_at,
                    source = excluded.source
                "#,
            )?;
            for identity in identities {
                stmt.execute(params![
                    identity.ecosystem,
                    identity.normalized_name,
                    approved_at,
                    source
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn enrich_info(&self, info: &mut runt_trust::TrustInfo) -> Result<()> {
        info.approved_uv_dependencies = self.approved_raw_specs("pypi", &info.uv_dependencies)?;
        info.approved_conda_dependencies =
            self.approved_raw_specs("conda", &info.conda_dependencies)?;
        info.approved_pixi_dependencies =
            self.approved_raw_specs("conda", &info.pixi_dependencies)?;
        info.approved_pixi_pypi_dependencies =
            self.approved_raw_specs("pypi", &info.pixi_pypi_dependencies)?;
        Ok(())
    }

    pub(crate) fn all_dependencies_approved(&self, info: &runt_trust::TrustInfo) -> Result<bool> {
        let identities = identities_from_trust_info(info);
        if identities.is_empty() {
            return Ok(false);
        }

        let StoreInner::Sqlite { conn } = self.inner.as_ref() else {
            return Ok(false);
        };
        let conn = conn
            .lock()
            .map_err(|_| anyhow::anyhow!("trusted package store mutex poisoned"))?;
        let mut stmt = conn.prepare(
            "SELECT 1 FROM trusted_packages WHERE ecosystem = ?1 AND normalized_name = ?2",
        )?;
        for identity in identities {
            let mut rows = stmt.query(params![identity.ecosystem, identity.normalized_name])?;
            if rows.next()?.is_none() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn approved_raw_specs(&self, ecosystem: &'static str, specs: &[String]) -> Result<Vec<String>> {
        let StoreInner::Sqlite { conn } = self.inner.as_ref() else {
            return Ok(vec![]);
        };
        let conn = conn
            .lock()
            .map_err(|_| anyhow::anyhow!("trusted package store mutex poisoned"))?;
        let mut stmt = conn.prepare(
            "SELECT 1 FROM trusted_packages WHERE ecosystem = ?1 AND normalized_name = ?2",
        )?;
        let mut approved = Vec::new();
        for spec in specs {
            let Some(name) = normalize_package_name(spec) else {
                continue;
            };
            let mut rows = stmt.query(params![ecosystem, name])?;
            if rows.next()?.is_some() {
                approved.push(spec.clone());
            }
        }
        Ok(approved)
    }
}

pub(crate) fn identities_from_trust_info(info: &runt_trust::TrustInfo) -> Vec<PackageIdentity> {
    let mut out = Vec::new();
    out.extend(identities_for_specs("pypi", &info.uv_dependencies));
    out.extend(identities_for_specs("conda", &info.conda_dependencies));
    out.extend(identities_for_specs("conda", &info.pixi_dependencies));
    out.extend(identities_for_specs("pypi", &info.pixi_pypi_dependencies));
    out
}

fn identities_for_specs(ecosystem: &'static str, specs: &[String]) -> Vec<PackageIdentity> {
    specs
        .iter()
        .filter_map(|raw_spec| {
            normalize_package_name(raw_spec).map(|normalized_name| PackageIdentity {
                ecosystem,
                raw_spec: raw_spec.clone(),
                normalized_name,
            })
        })
        .collect()
}

pub(crate) fn normalize_package_name(spec: &str) -> Option<String> {
    let mut name = spec.split(';').next().unwrap_or(spec).trim();
    if name.is_empty() {
        return None;
    }
    if let Some((_, after_channel)) = name.rsplit_once("::") {
        name = after_channel.trim();
    }
    if let Some((before_url, _)) = name.split_once('@') {
        name = before_url.trim();
    }
    let end = name
        .char_indices()
        .find_map(|(idx, ch)| {
            if matches!(ch, '[' | '<' | '>' | '=' | '!' | '~') || ch.is_whitespace() {
                Some(idx)
            } else {
                None
            }
        })
        .unwrap_or(name.len());
    name = name[..end].trim();
    if name.is_empty() {
        return None;
    }

    let mut normalized = String::with_capacity(name.len());
    let mut last_was_dash = false;
    for ch in name.chars() {
        let ch = ch.to_ascii_lowercase();
        if matches!(ch, '-' | '_' | '.') {
            if !last_was_dash {
                normalized.push('-');
                last_was_dash = true;
            }
        } else {
            normalized.push(ch);
            last_was_dash = false;
        }
    }
    let normalized = normalized.trim_matches('-').to_string();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn ensure_private_store_file(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        #[cfg(unix)]
        let existed = parent.exists();
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create trusted package store dir {}", parent.display()))?;
        #[cfg(unix)]
        if !existed {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("set permissions on {}", parent.display()))?;
        }
    }

    std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("create trusted package store {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("set permissions on {}", path.display()))?;
    }

    #[cfg(windows)]
    {
        apply_private_windows_acl(path)
            .with_context(|| format!("set private ACL on {}", path.display()))?;
    }

    Ok(())
}

#[cfg(windows)]
fn apply_private_windows_acl(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        SetFileSecurityW, DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    };

    let mut path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let sddl = "D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;OW)";
    let sddl_wide: Vec<u16> = std::ffi::OsStr::new(sddl)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mut security_descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();

    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl_wide.as_ptr(),
            SDDL_REVISION_1,
            &mut security_descriptor,
            std::ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(std::io::Error::last_os_error().into());
    }

    let set_result = unsafe {
        SetFileSecurityW(
            path_wide.as_mut_ptr(),
            DACL_SECURITY_INFORMATION,
            security_descriptor,
        )
    };
    unsafe {
        LocalFree(security_descriptor);
    }
    if set_result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

pub(crate) fn log_store_unavailable(store: &TrustedPackageStore) {
    if let Some(reason) = store.unavailable_reason() {
        warn!(
            "[trusted-packages] Package allowlist unavailable; auto-approval disabled: {}",
            reason
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_common_dependency_specs() {
        assert_eq!(normalize_package_name("pandas>=2"), Some("pandas".into()));
        assert_eq!(normalize_package_name("Pandas"), Some("pandas".into()));
        assert_eq!(
            normalize_package_name("scikit_learn"),
            Some("scikit-learn".into())
        );
        assert_eq!(
            normalize_package_name("requests[security,socks]>=2; python_version >= '3.11'"),
            Some("requests".into())
        );
        assert_eq!(
            normalize_package_name("conda-forge::NumPy=1.26"),
            Some("numpy".into())
        );
        assert_eq!(
            normalize_package_name("my.pkg__name>=1"),
            Some("my-pkg-name".into())
        );
    }

    #[test]
    fn store_inserts_idempotently_and_checks_by_ecosystem() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = TrustedPackageStore::open(tmp.path().join("trusted.sqlite")).unwrap();
        let info = runt_trust::TrustInfo {
            status: runt_trust::TrustStatus::Untrusted,
            uv_dependencies: vec!["Pandas>=2".into()],
            approved_uv_dependencies: vec![],
            conda_dependencies: vec!["numpy=1.26".into()],
            approved_conda_dependencies: vec![],
            conda_channels: vec![],
            pixi_dependencies: vec![],
            approved_pixi_dependencies: vec![],
            pixi_pypi_dependencies: vec![],
            approved_pixi_pypi_dependencies: vec![],
            pixi_channels: vec![],
        };

        store.add_from_info(&info, "test").unwrap();
        store.add_from_info(&info, "test").unwrap();

        assert!(store.all_dependencies_approved(&info).unwrap());

        let mut pypi_only = info.clone();
        pypi_only.conda_dependencies.clear();
        assert!(store.all_dependencies_approved(&pypi_only).unwrap());

        let conda_not_pypi = runt_trust::TrustInfo {
            status: runt_trust::TrustStatus::Untrusted,
            uv_dependencies: vec!["numpy".into()],
            approved_uv_dependencies: vec![],
            conda_dependencies: vec![],
            approved_conda_dependencies: vec![],
            conda_channels: vec![],
            pixi_dependencies: vec![],
            approved_pixi_dependencies: vec![],
            pixi_pypi_dependencies: vec![],
            approved_pixi_pypi_dependencies: vec![],
            pixi_channels: vec![],
        };
        assert!(!store.all_dependencies_approved(&conda_not_pypi).unwrap());
    }

    #[test]
    fn enriches_approved_raw_specs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = TrustedPackageStore::open(tmp.path().join("trusted.sqlite")).unwrap();
        let approved = runt_trust::TrustInfo {
            status: runt_trust::TrustStatus::Untrusted,
            uv_dependencies: vec!["pandas".into()],
            approved_uv_dependencies: vec![],
            conda_dependencies: vec![],
            approved_conda_dependencies: vec![],
            conda_channels: vec![],
            pixi_dependencies: vec![],
            approved_pixi_dependencies: vec![],
            pixi_pypi_dependencies: vec![],
            approved_pixi_pypi_dependencies: vec![],
            pixi_channels: vec![],
        };
        store.add_from_info(&approved, "test").unwrap();

        let mut mixed = approved.clone();
        mixed.uv_dependencies = vec!["pandas>=2".into(), "polars".into()];
        store.enrich_info(&mut mixed).unwrap();
        assert_eq!(mixed.approved_uv_dependencies, vec!["pandas>=2"]);
    }

    #[cfg(unix)]
    #[test]
    fn store_file_is_owner_only_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("trusted.sqlite");
        TrustedPackageStore::open(path.clone()).unwrap();
        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
