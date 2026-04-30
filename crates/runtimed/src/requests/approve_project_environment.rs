//! `NotebookRequest::ApproveProjectEnvironment` handler.
//!
//! Project environment approval is local setup consent. It records the
//! project file's packages in the existing local package allowlist, without
//! writing notebook dependency metadata or trust signatures.

use std::path::PathBuf;

use crate::notebook_sync_server::{environment_yml_trust_info, NotebookRoom};
use crate::protocol::NotebookResponse;
use tracing::warn;

pub(crate) async fn handle(
    room: &NotebookRoom,
    project_file_path: Option<String>,
) -> NotebookResponse {
    let detected = match resolve_project_file(room, project_file_path).await {
        Ok(detected) => detected,
        Err(error) => return NotebookResponse::Error { error },
    };

    let trust_info = match detected.kind {
        crate::project_file::ProjectFileKind::EnvironmentYml => {
            let config = match crate::project_file::parse_environment_yml(&detected.path) {
                Ok(config) => config,
                Err(error) => return NotebookResponse::Error { error },
            };
            environment_yml_trust_info(&config)
        }
        _ => {
            return NotebookResponse::Error {
                error: "Project environment approval is only supported for environment.yml"
                    .to_string(),
            };
        }
    };

    if let Err(error) = room
        .trusted_packages
        .add_from_info(&trust_info, "project_env_dialog")
    {
        warn!(
            "[trusted-packages] Failed to approve project environment {:?}: {}",
            detected.path, error
        );
        return NotebookResponse::Error {
            error: format!("Failed to approve project environment: {}", error),
        };
    }

    NotebookResponse::Ok {}
}

async fn resolve_project_file(
    room: &NotebookRoom,
    project_file_path: Option<String>,
) -> Result<crate::project_file::DetectedProjectFile, String> {
    if let Some(path) = project_file_path {
        let path = PathBuf::from(path);
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let kind = match filename {
            "environment.yml" | "environment.yaml" => {
                crate::project_file::ProjectFileKind::EnvironmentYml
            }
            _ => {
                return Err(format!(
                    "Unsupported project environment file '{}'",
                    path.to_string_lossy()
                ));
            }
        };
        return Ok(crate::project_file::DetectedProjectFile { path, kind });
    }

    let notebook_path = room.file_binding.path.read().await.clone();
    let working_dir = room.identity.working_dir.read().await.clone();
    let detection_path = notebook_path.as_ref().or(working_dir.as_ref());
    let Some(path) = detection_path else {
        return Err(
            "No notebook path or working directory for project environment approval".into(),
        );
    };
    crate::project_file::find_nearest_project_file(
        path,
        &[crate::project_file::ProjectFileKind::EnvironmentYml],
    )
    .ok_or_else(|| "No environment.yml found for project environment approval".to_string())
}
