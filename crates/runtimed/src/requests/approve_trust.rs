//! `NotebookRequest::ApproveTrust` handler.
//!
//! Trust approval is a semantic daemon operation: the daemon signs the current
//! dependency metadata and writes the resulting trust fields back into the
//! notebook CRDT. Callers never receive raw signature material to apply
//! themselves.

use crate::notebook_sync_server::{auto_sign_in_place, check_and_update_trust_state, NotebookRoom};
use crate::protocol::NotebookResponse;

const TRUST_APPROVAL_STALE_REASON: &str =
    "Dependencies changed while the trust dialog was open. Review before approving.";

pub(crate) async fn handle(
    room: &NotebookRoom,
    dependency_fingerprint: Option<String>,
) -> NotebookResponse {
    let persist_bytes = {
        let mut doc = room.doc.write().await;

        if let Err(error) = apply_trust_approval(&mut doc, dependency_fingerprint.as_deref()) {
            return error.into_response();
        }

        doc.save()
    };

    let _ = room.broadcasts.changed_tx.send(());
    if let Some(ref debouncer) = room.persistence.debouncer {
        let _ = debouncer.persist_tx.send(Some(persist_bytes));
    }

    check_and_update_trust_state(room).await;

    NotebookResponse::Ok {}
}

#[derive(Debug, PartialEq, Eq)]
enum TrustApprovalError {
    NoMetadata,
    StaleFingerprint,
    Sign(String),
    Write(String),
}

impl TrustApprovalError {
    fn into_response(self) -> NotebookResponse {
        match self {
            TrustApprovalError::NoMetadata => NotebookResponse::Error {
                error: "No metadata in Automerge doc".to_string(),
            },
            TrustApprovalError::StaleFingerprint => NotebookResponse::GuardRejected {
                reason: TRUST_APPROVAL_STALE_REASON.to_string(),
            },
            TrustApprovalError::Sign(error) | TrustApprovalError::Write(error) => {
                NotebookResponse::Error { error }
            }
        }
    }
}

fn apply_trust_approval(
    doc: &mut notebook_doc::NotebookDoc,
    dependency_fingerprint: Option<&str>,
) -> Result<(), TrustApprovalError> {
    let Some(mut snapshot) = doc.get_metadata_snapshot() else {
        return Err(TrustApprovalError::NoMetadata);
    };

    if let Some(expected) = dependency_fingerprint {
        let current = snapshot.dependency_fingerprint();
        if current != expected {
            return Err(TrustApprovalError::StaleFingerprint);
        }
    }

    auto_sign_in_place(&mut snapshot).map_err(TrustApprovalError::Sign)?;

    doc.set_metadata_snapshot(&snapshot)
        .map_err(|e| TrustApprovalError::Write(format!("Failed to write trust approval: {}", e)))
}

#[cfg(test)]
mod tests {
    use notebook_doc::{
        metadata::{NotebookMetadataSnapshot, UvInlineMetadata},
        NotebookDoc,
    };

    use super::*;

    fn doc_with_uv_deps(deps: &[&str]) -> NotebookDoc {
        let mut doc = NotebookDoc::new("trust-approval-test");
        let mut snapshot = NotebookMetadataSnapshot::default();
        snapshot.runt.uv = Some(UvInlineMetadata {
            dependencies: deps.iter().map(|dep| dep.to_string()).collect(),
            requires_python: None,
            prerelease: None,
        });
        doc.set_metadata_snapshot(&snapshot).unwrap();
        doc
    }

    #[test]
    fn approval_writes_trust_fields_to_the_doc() {
        let mut doc = doc_with_uv_deps(&["numpy"]);
        let fingerprint = doc.get_dependency_fingerprint().unwrap();

        apply_trust_approval(&mut doc, Some(&fingerprint)).unwrap();

        let approved = doc.get_metadata_snapshot().unwrap();
        assert!(approved.runt.trust_signature.is_some());
        assert!(approved.runt.trust_timestamp.is_some());
        let verified = crate::notebook_sync_server::verify_trust_from_snapshot(&approved);
        assert_eq!(verified.status, runt_trust::TrustStatus::Trusted);
    }

    #[test]
    fn approval_rejects_stale_dependency_fingerprint() {
        let mut doc = doc_with_uv_deps(&["numpy"]);

        let result = apply_trust_approval(&mut doc, Some("stale"));

        assert_eq!(result, Err(TrustApprovalError::StaleFingerprint));
        let snapshot = doc.get_metadata_snapshot().unwrap();
        assert!(snapshot.runt.trust_signature.is_none());
        assert!(snapshot.runt.trust_timestamp.is_none());
    }
}
