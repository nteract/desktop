//! Most-recently-used notebook list persisted to `recent.json`.
//!
//! Sits next to [`crate::settings_json_path`] and [`crate::session_state_path`]:
//! a dedicated file keeps the MRU list independent of `SyncedSettings`
//! (Automerge) and of the session restore file.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config_namespace;

/// Cap on retained entries. Excess are dropped from the tail.
pub const RECENT_MAX_ENTRIES: usize = 10;

const RECENT_SCHEMA_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct RecentNotebook {
    pub path: PathBuf,
    /// Milliseconds since the Unix epoch.
    pub last_opened_ms: u64,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct RecentNotebooks {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub entries: Vec<RecentNotebook>,
}

/// Path to `recent.json`, sibling of `settings.json`.
pub fn recent_notebooks_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(config_namespace())
        .join("recent.json")
}

/// Load the MRU list from disk. Returns [`RecentNotebooks::default`] on any
/// error (missing file, malformed JSON, I/O failure) — the list is soft state
/// and should never block the app.
pub fn load_recent() -> RecentNotebooks {
    load_from(&recent_notebooks_path())
}

fn load_from(path: &Path) -> RecentNotebooks {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return RecentNotebooks::default();
    };
    serde_json::from_str(&contents).unwrap_or_default()
}

fn save_to(path: &Path, value: &RecentNotebooks) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
    std::fs::write(path, format!("{json}\n"))
}

fn canonical_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Push `path` to the front of the MRU list (deduped by canonical path),
/// truncate to [`RECENT_MAX_ENTRIES`], and persist. Returns the new list.
pub fn record_open(path: &Path) -> RecentNotebooks {
    record_open_in(&recent_notebooks_path(), path)
}

fn record_open_in(store: &Path, path: &Path) -> RecentNotebooks {
    let mut recent = load_from(store);
    let key = canonical_key(path);
    recent.entries.retain(|e| canonical_key(&e.path) != key);
    recent.entries.insert(
        0,
        RecentNotebook {
            path: key,
            last_opened_ms: now_ms(),
        },
    );
    recent.entries.truncate(RECENT_MAX_ENTRIES);
    recent.schema_version = RECENT_SCHEMA_VERSION;
    let _ = save_to(store, &recent);
    recent
}

/// Remove a single entry (matched by canonical path) and persist.
pub fn remove_entry(path: &Path) -> RecentNotebooks {
    remove_entry_in(&recent_notebooks_path(), path)
}

fn remove_entry_in(store: &Path, path: &Path) -> RecentNotebooks {
    let mut recent = load_from(store);
    let key = canonical_key(path);
    recent.entries.retain(|e| canonical_key(&e.path) != key);
    recent.schema_version = RECENT_SCHEMA_VERSION;
    let _ = save_to(store, &recent);
    recent
}

/// Drop every entry and persist.
pub fn clear() -> RecentNotebooks {
    clear_in(&recent_notebooks_path())
}

fn clear_in(store: &Path) -> RecentNotebooks {
    let recent = RecentNotebooks {
        schema_version: RECENT_SCHEMA_VERSION,
        entries: Vec::new(),
    };
    let _ = save_to(store, &recent);
    recent
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_store() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recent.json");
        (dir, path)
    }

    #[test]
    fn load_missing_returns_default() {
        let (_dir, path) = tmp_store();
        let recent = load_from(&path);
        assert!(recent.entries.is_empty());
        assert_eq!(recent.schema_version, 0);
    }

    #[test]
    fn load_malformed_returns_default() {
        let (_dir, path) = tmp_store();
        fs::write(&path, "not json {{").unwrap();
        let recent = load_from(&path);
        assert!(recent.entries.is_empty());
    }

    #[test]
    fn record_puts_newest_first() {
        let (dir, path) = tmp_store();
        let a = dir.path().join("a.ipynb");
        let b = dir.path().join("b.ipynb");
        fs::write(&a, "{}").unwrap();
        fs::write(&b, "{}").unwrap();

        record_open_in(&path, &a);
        let recent = record_open_in(&path, &b);

        assert_eq!(recent.entries.len(), 2);
        assert_eq!(canonical_key(&recent.entries[0].path), canonical_key(&b));
        assert_eq!(canonical_key(&recent.entries[1].path), canonical_key(&a));
    }

    #[test]
    fn record_dedupes_by_canonical_path() {
        let (dir, path) = tmp_store();
        let a = dir.path().join("a.ipynb");
        fs::write(&a, "{}").unwrap();

        record_open_in(&path, &a);
        let recent = record_open_in(&path, &a);

        assert_eq!(recent.entries.len(), 1);
    }

    #[test]
    fn record_truncates_to_max() {
        let (dir, path) = tmp_store();
        for i in 0..(RECENT_MAX_ENTRIES + 5) {
            let p = dir.path().join(format!("nb-{i}.ipynb"));
            fs::write(&p, "{}").unwrap();
            record_open_in(&path, &p);
        }
        let recent = load_from(&path);
        assert_eq!(recent.entries.len(), RECENT_MAX_ENTRIES);
    }

    #[test]
    fn remove_drops_entry() {
        let (dir, path) = tmp_store();
        let a = dir.path().join("a.ipynb");
        let b = dir.path().join("b.ipynb");
        fs::write(&a, "{}").unwrap();
        fs::write(&b, "{}").unwrap();

        record_open_in(&path, &a);
        record_open_in(&path, &b);
        let recent = remove_entry_in(&path, &a);

        assert_eq!(recent.entries.len(), 1);
        assert_eq!(canonical_key(&recent.entries[0].path), canonical_key(&b));
    }

    #[test]
    fn clear_empties_list() {
        let (dir, path) = tmp_store();
        let a = dir.path().join("a.ipynb");
        fs::write(&a, "{}").unwrap();
        record_open_in(&path, &a);

        let recent = clear_in(&path);
        assert!(recent.entries.is_empty());

        let reloaded = load_from(&path);
        assert!(reloaded.entries.is_empty());
    }

    #[test]
    fn round_trips_through_disk() {
        let (dir, path) = tmp_store();
        let a = dir.path().join("a.ipynb");
        fs::write(&a, "{}").unwrap();
        record_open_in(&path, &a);

        let raw = fs::read_to_string(&path).unwrap();
        let parsed: RecentNotebooks = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.schema_version, RECENT_SCHEMA_VERSION);
        assert_eq!(parsed.entries.len(), 1);
    }
}
