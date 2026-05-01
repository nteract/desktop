//! `NotebookRequest::ApproveTrust` handler.
//!
//! Trust approval is a semantic daemon operation: the daemon signs the current
//! dependency metadata and writes the resulting trust fields back into the
//! notebook CRDT. Callers never receive raw signature material to apply
//! themselves.

use crate::notebook_sync_server::{
    auto_sign_in_place, check_and_update_trust_state, verify_trust_from_snapshot, NotebookRoom,
};
use crate::protocol::NotebookResponse;
use crate::requests::guarded;
use tracing::warn;

const TRUST_APPROVAL_STALE_REASON: &str =
    "Dependencies changed while the trust dialog was open. Review before approving.";

pub(crate) async fn handle(
    room: &NotebookRoom,
    observed_heads: Option<Vec<String>>,
) -> NotebookResponse {
    let (persist_bytes, trust_info) = {
        let mut doc = room.doc.write().await;

        let trust_info = match apply_trust_approval(&mut doc, observed_heads.as_deref()) {
            Ok(trust_info) => trust_info,
            Err(error) => return error.into_response(),
        };

        (doc.save(), trust_info)
    };

    if let Err(error) = room
        .trusted_packages
        .add_from_info(&trust_info, "trust_dialog")
    {
        warn!(
            "[trusted-packages] Failed to remember approved dependencies: {}",
            error
        );
    }

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
    StaleDependencies,
    Sign(String),
    Write(String),
}

impl TrustApprovalError {
    fn into_response(self) -> NotebookResponse {
        match self {
            TrustApprovalError::NoMetadata => NotebookResponse::Error {
                error: "No metadata in Automerge doc".to_string(),
            },
            TrustApprovalError::StaleDependencies => NotebookResponse::GuardRejected {
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
    observed_heads: Option<&[String]>,
) -> Result<runt_trust::TrustInfo, TrustApprovalError> {
    if let Some(observed_heads) = observed_heads {
        guarded::validate_dependencies_unchanged_since_observed(doc, observed_heads)
            .map_err(|_| TrustApprovalError::StaleDependencies)?;
    }

    let Some(mut snapshot) = doc.get_metadata_snapshot() else {
        return Err(TrustApprovalError::NoMetadata);
    };

    auto_sign_in_place(&mut snapshot).map_err(TrustApprovalError::Sign)?;
    let trust_info = verify_trust_from_snapshot(&snapshot).info;

    doc.set_metadata_snapshot(&snapshot)
        .map_err(|e| TrustApprovalError::Write(format!("Failed to write trust approval: {}", e)))?;

    Ok(trust_info)
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
        let observed_heads = doc.get_heads_hex();

        apply_trust_approval(&mut doc, Some(&observed_heads)).unwrap();

        let approved = doc.get_metadata_snapshot().unwrap();
        assert!(approved.runt.trust_signature.is_some());
        assert!(approved.runt.trust_timestamp.is_some());
        let verified = crate::notebook_sync_server::verify_trust_from_snapshot(&approved);
        assert_eq!(verified.status, runt_trust::TrustStatus::Trusted);
    }

    #[test]
    fn approval_rejects_stale_observed_dependencies() {
        let mut doc = doc_with_uv_deps(&["numpy"]);
        let observed_heads = doc.get_heads_hex();
        doc.add_uv_dependency("pandas").unwrap();

        let result = apply_trust_approval(&mut doc, Some(&observed_heads));

        assert!(matches!(result, Err(TrustApprovalError::StaleDependencies)));
        let snapshot = doc.get_metadata_snapshot().unwrap();
        assert!(snapshot.runt.trust_signature.is_none());
        assert!(snapshot.runt.trust_timestamp.is_none());
    }
}
