//! Guard helpers for trust-approved follow-up actions.

use std::str::FromStr;

use automerge::ChangeHash;
use notebook_doc::{diff::DocChangeset, NotebookDoc};

use crate::notebook_sync_server::{
    catch_automerge_panic, check_and_update_trust_state, NotebookRoom,
};
use crate::protocol::NotebookResponse;

#[derive(Debug)]
pub(crate) struct GuardRejection {
    reason: &'static str,
}

impl GuardRejection {
    pub(crate) fn into_response(self) -> NotebookResponse {
        NotebookResponse::GuardRejected {
            reason: self.reason.to_string(),
        }
    }
}

type GuardResult<T = ()> = Result<T, GuardRejection>;

pub(crate) async fn ensure_trusted(room: &NotebookRoom) -> GuardResult {
    check_and_update_trust_state(room).await;
    let trust_state = room.trust_state.read().await;
    if matches!(
        trust_state.status,
        runt_trust::TrustStatus::Trusted | runt_trust::TrustStatus::NoDependencies
    ) {
        Ok(())
    } else {
        Err(rejected(
            "Trust changed before the action could run. Review the notebook again.",
        ))
    }
}

pub(crate) fn validate_execute_cell(
    doc: &mut NotebookDoc,
    cell_id: &str,
    observed_heads: &[String],
) -> GuardResult {
    let changes = diff_from_observed(doc, observed_heads)?;
    if changes.cells.removed.iter().any(|id| id == cell_id)
        || changes.cells.added.iter().any(|id| id == cell_id)
    {
        return Err(rejected(
            "Cell changed or already ran. Review before running again.",
        ));
    }
    if changes.cells.changed.iter().any(|changed| {
        changed.cell_id == cell_id
            && (changed.fields.source || changed.fields.cell_type || changed.fields.outputs)
    }) {
        return Err(rejected(
            "Cell changed or already ran. Review before running again.",
        ));
    }
    Ok(())
}

pub(crate) fn validate_run_all(doc: &mut NotebookDoc, observed_heads: &[String]) -> GuardResult {
    if observed_heads.is_empty() {
        return Err(rejected(
            "Notebook state was not ready. Review before running again.",
        ));
    }
    let observed = parse_heads(observed_heads)?;
    let _ = diff_from_observed_heads(doc, &observed)?;
    let observed_code_cells = code_cell_guard_snapshots_at(doc, &observed);
    let current_code_cells = code_cell_guard_snapshots(doc);
    if observed_code_cells != current_code_cells {
        return Err(rejected(
            "Notebook changed before Run All could start. Review before running again.",
        ));
    }
    Ok(())
}

pub(crate) fn validate_sync_environment(
    doc: &mut NotebookDoc,
    observed_heads: &[String],
    dependency_fingerprint: &str,
) -> GuardResult {
    let _ = diff_from_observed(doc, observed_heads)?;
    let current = doc
        .get_dependency_fingerprint()
        .unwrap_or_else(|| serde_json::json!({}).to_string());
    if current != dependency_fingerprint {
        return Err(rejected(
            "Dependencies changed while the trust dialog was open. Review before syncing.",
        ));
    }
    Ok(())
}

fn diff_from_observed(
    doc: &mut NotebookDoc,
    observed_heads: &[String],
) -> GuardResult<DocChangeset> {
    if observed_heads.is_empty() {
        return Err(rejected(
            "Notebook state was not ready. Review before running again.",
        ));
    }
    let observed = parse_heads(observed_heads)?;
    diff_from_observed_heads(doc, &observed)
}

fn diff_from_observed_heads(
    doc: &mut NotebookDoc,
    observed: &[ChangeHash],
) -> GuardResult<DocChangeset> {
    let current = doc.get_heads();
    catch_automerge_panic("guarded-action-diff", || {
        notebook_doc::diff::diff_doc(doc.doc_mut(), observed, &current)
    })
    .map_err(|_| rejected("Notebook state changed unexpectedly. Review before running again."))
}

fn parse_heads(heads: &[String]) -> GuardResult<Vec<ChangeHash>> {
    heads
        .iter()
        .map(|head| {
            ChangeHash::from_str(head).map_err(|_| {
                rejected("Notebook state was not recognized. Review before running again.")
            })
        })
        .collect()
}

fn rejected(reason: &'static str) -> GuardRejection {
    GuardRejection { reason }
}

#[derive(Debug, PartialEq, Eq)]
struct CodeCellGuardSnapshot {
    cell_id: String,
    source: String,
    execution_id: Option<String>,
}

fn code_cell_guard_snapshots(doc: &NotebookDoc) -> Vec<CodeCellGuardSnapshot> {
    doc.get_cells()
        .into_iter()
        .filter(|cell| cell.cell_type == "code")
        .map(|cell| CodeCellGuardSnapshot {
            execution_id: doc.get_execution_id(&cell.id),
            cell_id: cell.id,
            source: cell.source,
        })
        .collect()
}

fn code_cell_guard_snapshots_at(
    doc: &NotebookDoc,
    heads: &[ChangeHash],
) -> Vec<CodeCellGuardSnapshot> {
    doc.get_cells_at_heads(heads)
        .into_iter()
        .filter(|cell| cell.cell_type == "code")
        .map(|cell| CodeCellGuardSnapshot {
            execution_id: doc.get_execution_id_at_heads(&cell.id, heads),
            cell_id: cell.id,
            source: cell.source,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use notebook_doc::{
        metadata::{NotebookMetadataSnapshot, UvInlineMetadata},
        NotebookDoc,
    };

    use super::*;

    fn code_doc() -> NotebookDoc {
        let mut doc = NotebookDoc::new("guard-test");
        doc.add_cell(0, "cell-1", "code").unwrap();
        doc.update_source("cell-1", "x = 1").unwrap();
        doc
    }

    fn two_code_cells_doc() -> NotebookDoc {
        let mut doc = NotebookDoc::new("guard-test");
        doc.add_cell(0, "cell-1", "code").unwrap();
        doc.update_source("cell-1", "x = 1").unwrap();
        doc.add_cell(1, "cell-2", "code").unwrap();
        doc.update_source("cell-2", "y = 2").unwrap();
        doc
    }

    fn set_uv_dependencies(doc: &mut NotebookDoc, deps: &[&str]) {
        let mut snapshot = doc.get_metadata_snapshot().unwrap_or_default();
        snapshot.runt.uv = Some(UvInlineMetadata {
            dependencies: deps.iter().map(|dep| dep.to_string()).collect(),
            requires_python: None,
            prerelease: None,
        });
        doc.set_metadata_snapshot(&snapshot).unwrap();
    }

    fn set_trust_metadata(doc: &mut NotebookDoc) {
        let mut snapshot = doc.get_metadata_snapshot().unwrap_or_default();
        snapshot.runt.trust_signature = Some("hmac-sha256:abc123".to_string());
        snapshot.runt.trust_timestamp = Some("2026-04-26T12:00:00Z".to_string());
        doc.set_metadata_snapshot(&snapshot).unwrap();
    }

    fn assert_rejected(result: GuardResult) {
        assert!(result.is_err());
    }

    #[test]
    fn execute_cell_allows_only_trust_metadata_after_observed_heads() {
        let mut doc = code_doc();
        set_uv_dependencies(&mut doc, &["numpy"]);
        let observed_heads = doc.get_heads_hex();

        set_trust_metadata(&mut doc);

        validate_execute_cell(&mut doc, "cell-1", &observed_heads).unwrap();
    }

    #[test]
    fn execute_cell_rejects_cell_mutations_after_observed_heads() {
        let mut source_changed = code_doc();
        let observed_heads = source_changed.get_heads_hex();
        source_changed.update_source("cell-1", "x = 2").unwrap();
        assert_rejected(validate_execute_cell(
            &mut source_changed,
            "cell-1",
            &observed_heads,
        ));

        let mut type_changed = code_doc();
        let observed_heads = type_changed.get_heads_hex();
        type_changed.set_cell_type("cell-1", "markdown").unwrap();
        assert_rejected(validate_execute_cell(
            &mut type_changed,
            "cell-1",
            &observed_heads,
        ));

        let mut removed = code_doc();
        let observed_heads = removed.get_heads_hex();
        removed.delete_cell("cell-1").unwrap();
        assert_rejected(validate_execute_cell(
            &mut removed,
            "cell-1",
            &observed_heads,
        ));

        let mut executed = code_doc();
        let observed_heads = executed.get_heads_hex();
        executed.set_execution_id("cell-1", Some("exec-1")).unwrap();
        assert_rejected(validate_execute_cell(
            &mut executed,
            "cell-1",
            &observed_heads,
        ));
    }

    #[test]
    fn guarded_actions_fail_closed_for_invalid_or_unknown_heads() {
        let mut doc = code_doc();
        assert_rejected(validate_execute_cell(
            &mut doc,
            "cell-1",
            &["not-a-change-hash".to_string()],
        ));

        let mut other = code_doc();
        other.update_source("cell-1", "z = 3").unwrap();
        let unknown_heads = other.get_heads_hex();
        assert_rejected(validate_execute_cell(&mut doc, "cell-1", &unknown_heads));
    }

    #[test]
    fn run_all_rejects_code_cell_changes_after_observed_heads() {
        let mut source_changed = two_code_cells_doc();
        let observed_heads = source_changed.get_heads_hex();
        source_changed.update_source("cell-2", "y = 3").unwrap();
        assert_rejected(validate_run_all(&mut source_changed, &observed_heads));

        let mut added = two_code_cells_doc();
        let observed_heads = added.get_heads_hex();
        added.add_cell(2, "cell-3", "code").unwrap();
        assert_rejected(validate_run_all(&mut added, &observed_heads));

        let mut reordered = two_code_cells_doc();
        let observed_heads = reordered.get_heads_hex();
        reordered.move_cell("cell-2", None).unwrap();
        assert_rejected(validate_run_all(&mut reordered, &observed_heads));

        let mut executed = two_code_cells_doc();
        let observed_heads = executed.get_heads_hex();
        executed.set_execution_id("cell-2", Some("exec-2")).unwrap();
        assert_rejected(validate_run_all(&mut executed, &observed_heads));
    }

    #[test]
    fn run_all_allows_non_code_changes_after_observed_heads() {
        let mut doc = two_code_cells_doc();
        doc.add_cell(2, "markdown-1", "markdown").unwrap();
        doc.update_source("markdown-1", "before").unwrap();
        let observed_heads = doc.get_heads_hex();

        doc.update_source("markdown-1", "after").unwrap();

        validate_run_all(&mut doc, &observed_heads).unwrap();
    }

    #[test]
    fn sync_environment_checks_dependency_fingerprint_not_trust_metadata() {
        let mut doc = code_doc();
        set_uv_dependencies(&mut doc, &["numpy"]);
        let observed_heads = doc.get_heads_hex();
        let dependency_fingerprint = doc.get_dependency_fingerprint().unwrap();

        set_trust_metadata(&mut doc);
        validate_sync_environment(&mut doc, &observed_heads, &dependency_fingerprint).unwrap();

        doc.add_uv_dependency("pandas").unwrap();
        assert_rejected(validate_sync_environment(
            &mut doc,
            &observed_heads,
            &dependency_fingerprint,
        ));
    }

    #[test]
    fn empty_dependency_fingerprint_is_stable_without_metadata() {
        let snapshot = NotebookMetadataSnapshot::default();
        assert_eq!(snapshot.dependency_fingerprint(), "{}");
    }
}
