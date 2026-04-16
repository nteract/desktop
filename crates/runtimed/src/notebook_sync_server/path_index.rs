//! Secondary index mapping canonical `.ipynb` paths to the UUID of the room
//! currently serving that file. Consulted by `open_notebook(path)` to reuse
//! an already-open room instead of creating a second one.
//!
//! **Invariant:** each canonical path maps to at most one UUID. `insert` that
//! would violate this returns `Err(PathIndexError::PathAlreadyOpen)` — the
//! caller decides whether to fail the request or merge (today: fail).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Default)]
pub struct PathIndex {
    inner: HashMap<PathBuf, Uuid>,
}

#[derive(Debug, thiserror::Error)]
pub enum PathIndexError {
    #[error("path already open in room {uuid}: {path}")]
    PathAlreadyOpen { uuid: Uuid, path: PathBuf },
}

impl PathIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lookup(&self, path: &Path) -> Option<Uuid> {
        self.inner.get(path).copied()
    }

    pub fn insert(&mut self, path: PathBuf, uuid: Uuid) -> Result<(), PathIndexError> {
        match self.inner.get(&path) {
            Some(&existing) if existing == uuid => Ok(()), // idempotent
            Some(&existing) => Err(PathIndexError::PathAlreadyOpen {
                uuid: existing,
                path,
            }),
            None => {
                self.inner.insert(path, uuid);
                Ok(())
            }
        }
    }

    pub fn remove(&mut self, path: &Path) -> Option<Uuid> {
        self.inner.remove(path)
    }

    pub fn remove_by_uuid(&mut self, uuid: Uuid) -> Option<PathBuf> {
        let path = self
            .inner
            .iter()
            .find(|(_, &u)| u == uuid)
            .map(|(p, _)| p.clone())?;
        self.inner.remove(&path);
        Some(path)
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn path(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn empty_index_returns_none_on_lookup() {
        let idx = PathIndex::new();
        assert!(idx.lookup(&path("/tmp/foo.ipynb")).is_none());
        assert!(idx.is_empty());
    }

    #[test]
    fn insert_then_lookup_returns_uuid() {
        let mut idx = PathIndex::new();
        let uuid = Uuid::new_v4();
        idx.insert(path("/tmp/foo.ipynb"), uuid).unwrap();
        assert_eq!(idx.lookup(&path("/tmp/foo.ipynb")), Some(uuid));
        assert_eq!(idx.len(), 1);
    }

    #[test]
    fn insert_same_uuid_twice_is_idempotent() {
        let mut idx = PathIndex::new();
        let uuid = Uuid::new_v4();
        idx.insert(path("/tmp/foo.ipynb"), uuid).unwrap();
        idx.insert(path("/tmp/foo.ipynb"), uuid).unwrap();
        assert_eq!(idx.len(), 1);
    }

    #[test]
    fn insert_conflicting_uuid_returns_error() {
        let mut idx = PathIndex::new();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        idx.insert(path("/tmp/foo.ipynb"), a).unwrap();
        let err = idx.insert(path("/tmp/foo.ipynb"), b).unwrap_err();
        match err {
            PathIndexError::PathAlreadyOpen { uuid, path: p } => {
                assert_eq!(uuid, a);
                assert_eq!(p, path("/tmp/foo.ipynb"));
            }
        }
    }

    #[test]
    fn remove_returns_uuid_and_clears_entry() {
        let mut idx = PathIndex::new();
        let uuid = Uuid::new_v4();
        idx.insert(path("/tmp/foo.ipynb"), uuid).unwrap();
        assert_eq!(idx.remove(&path("/tmp/foo.ipynb")), Some(uuid));
        assert!(idx.is_empty());
        assert!(idx.lookup(&path("/tmp/foo.ipynb")).is_none());
    }

    #[test]
    fn remove_missing_returns_none() {
        let mut idx = PathIndex::new();
        assert!(idx.remove(&path("/nope")).is_none());
    }

    #[test]
    fn remove_by_uuid_clears_entry() {
        let mut idx = PathIndex::new();
        let uuid = Uuid::new_v4();
        idx.insert(path("/tmp/foo.ipynb"), uuid).unwrap();
        assert_eq!(idx.remove_by_uuid(uuid), Some(path("/tmp/foo.ipynb")));
        assert!(idx.is_empty());
    }

    #[test]
    fn different_paths_with_different_uuids_coexist() {
        let mut idx = PathIndex::new();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        idx.insert(path("/tmp/a.ipynb"), a).unwrap();
        idx.insert(path("/tmp/b.ipynb"), b).unwrap();
        assert_eq!(idx.lookup(&path("/tmp/a.ipynb")), Some(a));
        assert_eq!(idx.lookup(&path("/tmp/b.ipynb")), Some(b));
        assert_eq!(idx.len(), 2);
    }
}
