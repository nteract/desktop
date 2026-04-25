//! Trust allowlist: the set of `(package_manager, package_name)` pairs
//! the user has already approved for auto-launch.
//!
//! # Storage model
//!
//! Durability via the `turso` crate — the Turso team's ground-up
//! pure-Rust rewrite of the SQLite engine, SQLite-compatible on the
//! wire + file format. One table:
//!
//! ```sql
//! CREATE TABLE trust_allowlist (
//!   manager   TEXT NOT NULL,
//!   name      TEXT NOT NULL,
//!   added_at  INTEGER NOT NULL,
//!   PRIMARY KEY (manager, name)
//! );
//! ```
//!
//! `added_at` is Unix seconds. (Note: `turso` 0.5 does not yet parse
//! `WITHOUT ROWID`, so the primary key goes on a normal b-tree over
//! rowid. Functionally identical for our lookup pattern, since we
//! always hit the `(manager, name)` index. Reintroduce once turso
//! catches up to SQLite's full DDL.)
//!
//! # Hot path
//!
//! All rows are loaded into an in-memory `HashSet` at `open()` time.
//! `contains()` is a pure set lookup — no disk I/O, no `.await`. The
//! set is the authoritative view inside the running daemon; turso is
//! the durability layer and source of truth on cold start.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use parking_lot::RwLock;
use thiserror::Error;
use turso::{params, Builder};

const DB_FILE: &str = "allowlist.db";

/// Package manager subset the trust surface covers today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PackageManager {
    Uv,
    Conda,
    Pixi,
}

impl PackageManager {
    pub fn as_str(self) -> &'static str {
        match self {
            PackageManager::Uv => "uv",
            PackageManager::Conda => "conda",
            PackageManager::Pixi => "pixi",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "uv" => Some(PackageManager::Uv),
            "conda" => Some(PackageManager::Conda),
            "pixi" => Some(PackageManager::Pixi),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedPackage {
    pub manager: PackageManager,
    pub name: String,
    pub added_at: i64,
}

#[derive(Debug, Error)]
pub enum TrustAllowlistError {
    #[error("turso error: {0}")]
    Turso(#[from] turso::Error),
    #[error("store directory unavailable")]
    NoStoreDir,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("system clock before unix epoch: {0}")]
    Clock(#[from] std::time::SystemTimeError),
}

/// In-memory view of the allowlist backed by a turso database on disk.
#[derive(Clone)]
pub struct TrustAllowlist {
    entries: Arc<RwLock<HashSet<(PackageManager, String)>>>,
    db: Arc<turso::Database>,
}

impl TrustAllowlist {
    /// Open (or create) the allowlist at `store_dir`. Creates the
    /// directory + database file if missing, applies the schema, and
    /// loads every row into the in-memory set.
    pub async fn open(store_dir: &Path) -> Result<Self, TrustAllowlistError> {
        tokio::fs::create_dir_all(store_dir).await?;
        let db_path = store_dir.join(DB_FILE);
        let db_path_str = db_path
            .to_str()
            .ok_or(TrustAllowlistError::NoStoreDir)?
            .to_string();
        let db = Builder::new_local(&db_path_str).build().await?;
        let conn = db.connect()?;

        // turso 0.5 uses WAL by default and doesn't expose a
        // `PRAGMA journal_mode` yet — it's the engine's built-in
        // durability mode. No pragma batch needed.

        conn.execute(
            "CREATE TABLE IF NOT EXISTS trust_allowlist (\n\
               manager  TEXT NOT NULL,\n\
               name     TEXT NOT NULL,\n\
               added_at INTEGER NOT NULL,\n\
               PRIMARY KEY (manager, name)\n\
             )",
            (),
        )
        .await?;

        let mut entries = HashSet::new();
        let mut rows = conn
            .query("SELECT manager, name FROM trust_allowlist", ())
            .await?;
        while let Some(row) = rows.next().await? {
            let manager: String = row.get(0)?;
            let name: String = row.get(1)?;
            if let Some(manager) = PackageManager::parse(&manager) {
                entries.insert((manager, name));
            }
        }

        Ok(Self {
            entries: Arc::new(RwLock::new(entries)),
            db: Arc::new(db),
        })
    }

    /// Hot path: does the `(manager, name)` pair live in the allowlist?
    /// Pure in-memory lookup — no disk I/O, no `.await`.
    pub fn contains(&self, manager: PackageManager, name: &str) -> bool {
        self.entries.read().contains(&(manager, name.to_string()))
    }

    /// Return candidates that are NOT already on the allowlist. Used
    /// by the dialog's partial-coverage UI.
    pub fn novel<'a, I>(&self, candidates: I) -> Vec<(PackageManager, String)>
    where
        I: IntoIterator<Item = (PackageManager, &'a str)>,
    {
        let guard = self.entries.read();
        candidates
            .into_iter()
            .filter(|(manager, name)| !guard.contains(&(*manager, name.to_string())))
            .map(|(manager, name)| (manager, name.to_string()))
            .collect()
    }

    /// Append a batch of packages to the allowlist. Already-present
    /// entries are skipped via the in-memory dedup; DB upserts use
    /// `OR IGNORE` as defense-in-depth.
    pub async fn add(
        &self,
        manager: PackageManager,
        names: &[String],
    ) -> Result<(), TrustAllowlistError> {
        if names.is_empty() {
            return Ok(());
        }
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_secs() as i64;

        let fresh: Vec<String> = {
            let mut guard = self.entries.write();
            names
                .iter()
                .filter_map(|n| {
                    if guard.insert((manager, n.clone())) {
                        Some(n.clone())
                    } else {
                        None
                    }
                })
                .collect()
        };
        if fresh.is_empty() {
            return Ok(());
        }

        let mut conn = self.db.connect()?;
        // Batch the appends in a single transaction so the write is
        // atomic — either all rows land or none do. turso uses WAL
        // internally so the per-commit cost is one fsync.
        let tx = conn.transaction().await?;
        for name in &fresh {
            tx.execute(
                "INSERT OR IGNORE INTO trust_allowlist (manager, name, added_at) VALUES (?1, ?2, ?3)",
                params![manager.as_str().to_string(), name.clone(), now],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Remove a single entry. No-op when it's not present.
    pub async fn remove(
        &self,
        manager: PackageManager,
        name: &str,
    ) -> Result<(), TrustAllowlistError> {
        let removed = {
            let mut guard = self.entries.write();
            guard.remove(&(manager, name.to_string()))
        };
        if !removed {
            return Ok(());
        }
        let conn = self.db.connect()?;
        conn.execute(
            "DELETE FROM trust_allowlist WHERE manager = ?1 AND name = ?2",
            params![manager.as_str().to_string(), name.to_string()],
        )
        .await?;
        Ok(())
    }

    /// Snapshot names for a given manager (sorted).
    pub fn list(&self, manager: PackageManager) -> Vec<String> {
        let guard = self.entries.read();
        let mut out: Vec<String> = guard
            .iter()
            .filter(|(m, _)| *m == manager)
            .map(|(_, n)| n.clone())
            .collect();
        out.sort();
        out
    }

    /// Snapshot with timestamps. Heavier than `list` — reads from the DB.
    pub async fn list_all(&self) -> Result<Vec<TrustedPackage>, TrustAllowlistError> {
        let conn = self.db.connect()?;
        let mut rows = conn
            .query(
                "SELECT manager, name, added_at FROM trust_allowlist ORDER BY manager, name",
                (),
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            let manager: String = row.get(0)?;
            let name: String = row.get(1)?;
            let added_at: i64 = row.get(2)?;
            if let Some(manager) = PackageManager::parse(&manager) {
                out.push(TrustedPackage {
                    manager,
                    name,
                    added_at,
                });
            }
        }
        Ok(out)
    }

    /// Drop all entries. Used by tests and by an eventual "forget"
    /// action in the UI.
    pub async fn clear(&self) -> Result<(), TrustAllowlistError> {
        self.entries.write().clear();
        let conn = self.db.connect()?;
        conn.execute("DELETE FROM trust_allowlist", ()).await?;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.read().is_empty()
    }
}
