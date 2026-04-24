//! Trust allowlist: the set of `(package_manager, package_name)` pairs
//! the user has already approved for auto-launch.
//!
//! # Storage model
//!
//! Durability via LanceDB, one table `trust_allowlist` with schema:
//!
//! | column | type | notes |
//! |--------|------|-------|
//! | `manager` | Utf8 | "uv" / "conda" / "pixi" |
//! | `name` | Utf8 | package name, no version specifier |
//! | `added_at` | Int64 | Unix seconds |
//!
//! Hot path goes through an in-memory `HashSet` loaded at `open()`
//! time. `contains()` never touches disk. Writes update the set *and*
//! append to LanceDB in the same call, so a crash between them
//! leaves the set as the more permissive view (nothing committed to
//! disk, so the next process reload is consistent).
//!
//! # What the spike answers
//!
//! - Cold-start load latency at various table sizes.
//! - Append latency (`add()`), including fsync.
//! - Per-call overhead of the in-memory wrapper.
//!
//! Benchmarks live in `benches/trust_allowlist.rs`.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use arrow_array::{
    Array, Int64Array, RecordBatch, RecordBatchIterator, RecordBatchReader, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use futures_util::TryStreamExt;
use lancedb::query::ExecutableQuery;
use lancedb::{Connection, Table};
use parking_lot::RwLock;
use thiserror::Error;

const TABLE_NAME: &str = "trust_allowlist";

/// The subset of package managers the trust surface covers today.
///
/// Matches `notebook_protocol::connection::PackageManager` but
/// intentionally does not depend on the larger protocol crate — this
/// store is meant to stay free of daemon types so the facade can be
/// reused.
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

/// A single trusted (manager, package) entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedPackage {
    pub manager: PackageManager,
    pub name: String,
    pub added_at: i64,
}

#[derive(Debug, Error)]
pub enum TrustAllowlistError {
    #[error("lancedb error: {0}")]
    Lance(#[from] lancedb::Error),
    #[error("arrow error: {0}")]
    Arrow(#[from] arrow_schema::ArrowError),
    #[error("store directory is unavailable")]
    NoStoreDir,
    #[error("system clock before unix epoch: {0}")]
    Clock(#[from] std::time::SystemTimeError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// In-memory view of the allowlist backed by a LanceDB table on disk.
///
/// Cloneable (via `Arc`) so multiple daemon tasks can share one handle.
#[derive(Clone)]
pub struct TrustAllowlist {
    entries: Arc<RwLock<HashSet<(PackageManager, String)>>>,
    conn: Connection,
    table: Table,
}

impl TrustAllowlist {
    /// Open (or create) the allowlist at `store_dir`. Creates the
    /// directory if it does not exist. Loads all rows into the
    /// in-memory set.
    pub async fn open(store_dir: &Path) -> Result<Self, TrustAllowlistError> {
        tokio::fs::create_dir_all(store_dir).await?;
        let uri = store_dir
            .to_str()
            .ok_or(TrustAllowlistError::NoStoreDir)?
            .to_string();

        let conn = lancedb::connect(&uri).execute().await?;
        let table = match conn.open_table(TABLE_NAME).execute().await {
            Ok(t) => t,
            Err(lancedb::Error::TableNotFound { .. }) => {
                conn.create_empty_table(TABLE_NAME, schema())
                    .execute()
                    .await?
            }
            Err(e) => return Err(e.into()),
        };

        let entries = load_entries(&table).await?;
        Ok(Self {
            entries: Arc::new(RwLock::new(entries)),
            conn,
            table,
        })
    }

    /// Hot path: does the (manager, name) pair live in the allowlist?
    /// Pure in-memory lookup — no disk I/O, no `.await`.
    pub fn contains(&self, manager: PackageManager, name: &str) -> bool {
        self.entries.read().contains(&(manager, name.to_string()))
    }

    /// Filter a collection of candidate (manager, name) pairs, returning
    /// only those NOT already on the allowlist. Useful for the trust
    /// dialog's "novel packages" view.
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

    /// Add a batch of packages to the allowlist. Already-present entries
    /// are skipped (dedup via the in-memory set), so callers can pass
    /// the whole trusted set from a notebook without worrying.
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

        let batch = build_batch(manager, &fresh, now)?;
        let schema = batch.schema();
        let reader = RecordBatchIterator::new(vec![Ok(batch)].into_iter(), schema);
        let boxed: Box<dyn RecordBatchReader + Send> = Box::new(reader);
        self.table.add(boxed).execute().await?;
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
        let predicate = format!(
            "manager = '{}' AND name = '{}'",
            manager.as_str(),
            // SQL single-quote escape — collapse to defense-in-depth
            // for the unlikely case that a package name slips past
            // PEP-508 / conda name rules and carries a quote. Real
            // inputs come from trusted daemon paths.
            name.replace('\'', "''")
        );
        self.table.delete(&predicate).await?;
        Ok(())
    }

    /// Snapshot all entries for a given manager.
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

    /// Snapshot of the entire allowlist with timestamps from the
    /// in-memory view only (timestamps are not cached in the set).
    /// Heavier than `list` — reads from LanceDB.
    pub async fn list_all(&self) -> Result<Vec<TrustedPackage>, TrustAllowlistError> {
        load_entries_full(&self.table).await
    }

    /// Drop all entries. Used in tests and by an eventual "forget"
    /// action in the UI.
    pub async fn clear(&self) -> Result<(), TrustAllowlistError> {
        self.entries.write().clear();
        // `delete` with a trivially-true predicate clears every row.
        self.table.delete("true").await?;
        Ok(())
    }

    /// Number of entries in the in-memory view.
    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.read().is_empty()
    }

    /// Exposes the underlying connection for tests / follow-up tables.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("manager", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("added_at", DataType::Int64, false),
    ]))
}

fn build_batch(
    manager: PackageManager,
    names: &[String],
    added_at: i64,
) -> Result<RecordBatch, arrow_schema::ArrowError> {
    let managers = StringArray::from(vec![manager.as_str(); names.len()]);
    let names_arr = StringArray::from_iter_values(names.iter().map(|s| s.as_str()));
    let timestamps = Int64Array::from(vec![added_at; names.len()]);
    RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(managers),
            Arc::new(names_arr),
            Arc::new(timestamps),
        ],
    )
}

async fn load_entries(
    table: &Table,
) -> Result<HashSet<(PackageManager, String)>, TrustAllowlistError> {
    let mut out = HashSet::new();
    let mut stream = table.query().execute().await?;
    while let Some(batch) = stream.try_next().await? {
        push_entries(&batch, &mut out);
    }
    Ok(out)
}

async fn load_entries_full(table: &Table) -> Result<Vec<TrustedPackage>, TrustAllowlistError> {
    let mut out = Vec::new();
    let mut stream = table.query().execute().await?;
    while let Some(batch) = stream.try_next().await? {
        push_entries_full(&batch, &mut out);
    }
    Ok(out)
}

fn push_entries(batch: &RecordBatch, out: &mut HashSet<(PackageManager, String)>) {
    let Some(managers) = batch
        .column_by_name("manager")
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
    else {
        return;
    };
    let Some(names) = batch
        .column_by_name("name")
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
    else {
        return;
    };
    for i in 0..batch.num_rows() {
        if managers.is_null(i) || names.is_null(i) {
            continue;
        }
        if let Some(manager) = PackageManager::parse(managers.value(i)) {
            out.insert((manager, names.value(i).to_string()));
        }
    }
}

fn push_entries_full(batch: &RecordBatch, out: &mut Vec<TrustedPackage>) {
    let Some(managers) = batch
        .column_by_name("manager")
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
    else {
        return;
    };
    let Some(names) = batch
        .column_by_name("name")
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
    else {
        return;
    };
    let Some(timestamps) = batch
        .column_by_name("added_at")
        .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
    else {
        return;
    };
    for i in 0..batch.num_rows() {
        if managers.is_null(i) || names.is_null(i) || timestamps.is_null(i) {
            continue;
        }
        if let Some(manager) = PackageManager::parse(managers.value(i)) {
            out.push(TrustedPackage {
                manager,
                name: names.value(i).to_string(),
                added_at: timestamps.value(i),
            });
        }
    }
}
