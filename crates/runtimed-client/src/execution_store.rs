//! Durable execution-result store.
//!
//! RuntimeStateDoc remains the live, daemon-authoritative sync document. This
//! store is a small sidecar ledger for terminal executions so tools can recover
//! `get_results(execution_id)` after a room is evicted.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const EXECUTION_RECORD_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecutionRecord {
    pub schema_version: u32,
    pub execution_id: String,
    pub context_kind: String,
    pub context_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notebook_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell_id: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(default)]
    pub outputs: Vec<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ExecutionRecord {
    pub fn from_execution_state(
        execution_id: &str,
        context_kind: impl Into<String>,
        context_id: impl Into<String>,
        notebook_path: Option<String>,
        exec: &runtime_doc::ExecutionState,
    ) -> Self {
        let now = Utc::now();
        Self {
            schema_version: EXECUTION_RECORD_SCHEMA_VERSION,
            execution_id: execution_id.to_string(),
            context_kind: context_kind.into(),
            context_id: context_id.into(),
            notebook_path,
            cell_id: Some(exec.cell_id.clone()),
            status: exec.status.clone(),
            success: exec.success,
            execution_count: exec.execution_count,
            source: exec.source.clone(),
            seq: exec.seq,
            outputs: exec.outputs.clone(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self.status.as_str(), "done" | "error")
    }

    pub fn matches_notebook_cell(
        &self,
        context_id: &str,
        notebook_path: Option<&str>,
        cell_id: &str,
        source: &str,
        execution_count: Option<i64>,
        outputs: &[serde_json::Value],
    ) -> bool {
        self.schema_version == EXECUTION_RECORD_SCHEMA_VERSION
            && self.context_kind == "notebook"
            && self.context_id == context_id
            && self.notebook_path.as_deref() == notebook_path
            && self.cell_id.as_deref() == Some(cell_id)
            && self.source.as_deref() == Some(source)
            && self.execution_count == execution_count
            && outputs_match_for_reload(&self.outputs, outputs)
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionStore {
    root: PathBuf,
}

impl ExecutionStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub async fn write_record(&self, mut record: ExecutionRecord) -> std::io::Result<()> {
        validate_execution_id(&record.execution_id)?;
        tokio::fs::create_dir_all(&self.root).await?;
        if let Some(existing) = self.read_record(&record.execution_id).await {
            record.created_at = existing.created_at;
        }
        record.updated_at = Utc::now();
        let path = self.path_for_id(&record.execution_id)?;
        let tmp = self.root.join(format!(
            "{}.{}.tmp",
            record.execution_id,
            uuid::Uuid::new_v4()
        ));
        let bytes = serde_json::to_vec_pretty(&record).map_err(std::io::Error::other)?;
        tokio::fs::write(&tmp, bytes).await?;
        tokio::fs::rename(&tmp, &path).await?;
        Ok(())
    }

    pub async fn read_record(&self, execution_id: &str) -> Option<ExecutionRecord> {
        let path = self.path_for_id(execution_id).ok()?;
        let bytes = tokio::fs::read(&path).await.ok()?;
        match serde_json::from_slice::<ExecutionRecord>(&bytes) {
            Ok(record) if record.schema_version == EXECUTION_RECORD_SCHEMA_VERSION => Some(record),
            Ok(_) => None,
            Err(e) => {
                log::warn!(
                    "[execution-store] Ignoring corrupt execution record {:?}: {}",
                    path,
                    e
                );
                None
            }
        }
    }

    pub async fn list_records(&self) -> Vec<ExecutionRecord> {
        let mut records = Vec::new();
        let mut entries = match tokio::fs::read_dir(&self.root).await {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return records,
            Err(e) => {
                log::warn!(
                    "[execution-store] Failed to read execution store {:?}: {}",
                    self.root,
                    e
                );
                return records;
            }
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            match tokio::fs::read(&path).await {
                Ok(bytes) => match serde_json::from_slice::<ExecutionRecord>(&bytes) {
                    Ok(record) if record.schema_version == EXECUTION_RECORD_SCHEMA_VERSION => {
                        records.push(record);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        log::warn!(
                            "[execution-store] Ignoring corrupt execution record {:?}: {}",
                            path,
                            e
                        );
                    }
                },
                Err(e) => {
                    log::warn!(
                        "[execution-store] Failed to read execution record {:?}: {}",
                        path,
                        e
                    );
                }
            }
        }
        records
    }

    pub async fn list_context(&self, context_kind: &str, context_id: &str) -> Vec<ExecutionRecord> {
        self.list_records()
            .await
            .into_iter()
            .filter(|record| record.context_kind == context_kind && record.context_id == context_id)
            .collect()
    }

    pub async fn find_matching_notebook_cell(
        &self,
        context_id: &str,
        notebook_path: Option<&str>,
        cell_id: &str,
        source: &str,
        execution_count: Option<i64>,
        outputs: &[serde_json::Value],
    ) -> Option<ExecutionRecord> {
        self.list_context("notebook", context_id)
            .await
            .into_iter()
            .find(|record| {
                record.matches_notebook_cell(
                    context_id,
                    notebook_path,
                    cell_id,
                    source,
                    execution_count,
                    outputs,
                )
            })
    }

    pub async fn prune_older_than(&self, cutoff: DateTime<Utc>) -> usize {
        let mut removed = 0usize;
        for record in self.list_records().await {
            if record.updated_at >= cutoff {
                continue;
            }
            if let Ok(path) = self.path_for_id(&record.execution_id) {
                match tokio::fs::remove_file(&path).await {
                    Ok(()) => removed += 1,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => {
                        log::warn!(
                            "[execution-store] Failed to prune execution record {:?}: {}",
                            path,
                            e
                        );
                    }
                }
            }
        }
        removed
    }

    pub async fn referenced_blob_hashes(&self) -> HashSet<String> {
        let mut hashes = HashSet::new();
        for record in self.list_records().await {
            for output in &record.outputs {
                collect_blob_hashes(output, &mut hashes);
            }
        }
        hashes
    }

    fn path_for_id(&self, execution_id: &str) -> std::io::Result<PathBuf> {
        validate_execution_id(execution_id)?;
        Ok(self.root.join(format!("{execution_id}.json")))
    }
}

fn validate_execution_id(execution_id: &str) -> std::io::Result<()> {
    if execution_id.is_empty()
        || !execution_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid execution_id for file-backed store: {execution_id:?}"),
        ));
    }
    Ok(())
}

fn outputs_match_for_reload(a: &[serde_json::Value], b: &[serde_json::Value]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(left, right)| {
            normalize_output_for_match(left) == normalize_output_for_match(right)
        })
}

fn normalize_output_for_match(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(obj) => {
            let mut normalized = serde_json::Map::new();
            for (key, val) in obj {
                if matches!(key.as_str(), "output_id" | "llm_preview" | "rich") {
                    continue;
                }
                normalized.insert(key.clone(), normalize_output_for_match(val));
            }
            serde_json::Value::Object(normalized)
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(normalize_output_for_match).collect())
        }
        other => other.clone(),
    }
}

fn collect_blob_hashes(
    manifest: &serde_json::Value,
    hashes: &mut std::collections::HashSet<String>,
) {
    if let Some(data) = manifest.get("data").and_then(|d| d.as_object()) {
        for mime_data in data.values() {
            collect_blob_hashes_recursive(mime_data, hashes);
        }
    }
    if let Some(text) = manifest.get("text") {
        collect_blob_hashes_recursive(text, hashes);
    }
    if let Some(traceback) = manifest.get("traceback") {
        collect_blob_hashes_recursive(traceback, hashes);
    }
}

fn collect_blob_hashes_recursive(
    value: &serde_json::Value,
    hashes: &mut std::collections::HashSet<String>,
) {
    match value {
        serde_json::Value::Object(obj) => {
            if let Some(hash) = obj.get("blob").and_then(|b| b.as_str()) {
                hashes.insert(hash.to_string());
            }
            for value in obj.values() {
                collect_blob_hashes_recursive(value, hashes);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_blob_hashes_recursive(value, hashes);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str) -> ExecutionRecord {
        ExecutionRecord {
            schema_version: EXECUTION_RECORD_SCHEMA_VERSION,
            execution_id: id.to_string(),
            context_kind: "notebook".to_string(),
            context_id: "/tmp/a.ipynb".to_string(),
            notebook_path: Some("/tmp/a.ipynb".to_string()),
            cell_id: Some("cell-1".to_string()),
            status: "done".to_string(),
            success: Some(true),
            execution_count: Some(1),
            source: Some("1 + 1".to_string()),
            seq: Some(0),
            outputs: vec![serde_json::json!({
                "output_type": "stream",
                "output_id": "old",
                "text": {"blob": "abc", "size": 3}
            })],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn write_and_read_round_trips() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = ExecutionStore::new(tmp.path());
        store.write_record(record("exec-1")).await.unwrap();

        let loaded = store.read_record("exec-1").await.unwrap();
        assert_eq!(loaded.execution_id, "exec-1");
        assert_eq!(loaded.outputs.len(), 1);
    }

    #[tokio::test]
    async fn corrupt_records_are_ignored() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = ExecutionStore::new(tmp.path());
        tokio::fs::write(tmp.path().join("exec-1.json"), b"{not json")
            .await
            .unwrap();

        assert!(store.read_record("exec-1").await.is_none());
        assert!(store.list_records().await.is_empty());
    }

    #[tokio::test]
    async fn list_context_filters_records() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = ExecutionStore::new(tmp.path());
        store.write_record(record("exec-1")).await.unwrap();
        let mut other = record("exec-2");
        other.context_id = "/tmp/b.ipynb".to_string();
        other.notebook_path = Some("/tmp/b.ipynb".to_string());
        store.write_record(other).await.unwrap();

        let records = store.list_context("notebook", "/tmp/a.ipynb").await;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].execution_id, "exec-1");
    }

    #[tokio::test]
    async fn find_matching_notebook_cell_ignores_runtime_only_output_fields() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = ExecutionStore::new(tmp.path());
        store.write_record(record("exec-1")).await.unwrap();

        let loaded = store
            .find_matching_notebook_cell(
                "/tmp/a.ipynb",
                Some("/tmp/a.ipynb"),
                "cell-1",
                "1 + 1",
                Some(1),
                &[serde_json::json!({
                    "output_type": "stream",
                    "output_id": "new",
                    "llm_preview": "preview",
                    "text": {"blob": "abc", "size": 3}
                })],
            )
            .await
            .unwrap();
        assert_eq!(loaded.execution_id, "exec-1");
    }

    #[tokio::test]
    async fn prune_removes_old_records() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = ExecutionStore::new(tmp.path());
        let mut old = record("exec-1");
        old.updated_at = Utc::now() - chrono::Duration::days(31);
        tokio::fs::create_dir_all(tmp.path()).await.unwrap();
        tokio::fs::write(
            tmp.path().join("exec-1.json"),
            serde_json::to_vec_pretty(&old).unwrap(),
        )
        .await
        .unwrap();

        let removed = store
            .prune_older_than(Utc::now() - chrono::Duration::days(30))
            .await;
        assert_eq!(removed, 1);
        assert!(store.read_record("exec-1").await.is_none());
    }

    #[tokio::test]
    async fn referenced_blob_hashes_collects_output_refs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = ExecutionStore::new(tmp.path());
        store.write_record(record("exec-1")).await.unwrap();

        let hashes = store.referenced_blob_hashes().await;
        assert!(hashes.contains("abc"));
    }
}
