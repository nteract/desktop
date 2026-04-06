//! Notebook trust verification using HMAC signatures over dependency metadata.
//!
//! # Security Model
//!
//! Notebooks can embed arbitrary package dependencies that get installed with full
//! OS permissions when a kernel starts. This creates an attack vector: a malicious
//! notebook could trigger installation of malware via `setup.py`.
//!
//! To mitigate this, we sign the dependency-related metadata fields with a per-machine
//! HMAC key. Only notebooks created or approved on this machine will have valid signatures.
//!
//! Key insight: we sign ONLY the dependency metadata, not cell contents. This means:
//! - Editing code in cells: notebook stays trusted
//! - External modification of dependencies: requires re-approval

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::path::PathBuf;

type HmacSha256 = Hmac<Sha256>;

/// Result of verifying a notebook's trust status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TrustStatus {
    /// Notebook has a valid signature matching current dependencies.
    Trusted,

    /// Notebook has no signature (new or external notebook).
    Untrusted,

    /// Notebook has a signature but it doesn't match current dependencies.
    /// This indicates external modification of the dependency fields.
    SignatureInvalid,

    /// No dependencies configured, no trust check needed.
    NoDependencies,
}

/// Information about notebook trust for the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustInfo {
    pub status: TrustStatus,
    /// The UV dependencies that will be installed (if any).
    pub uv_dependencies: Vec<String>,
    /// The conda dependencies that will be installed (if any).
    pub conda_dependencies: Vec<String>,
    /// Conda channels configured.
    pub conda_channels: Vec<String>,
}

/// Path to the trust key file.
///
/// In tests, this can be overridden by setting RUNT_TRUST_KEY_PATH environment variable.
fn trust_key_path() -> Option<PathBuf> {
    // Allow override for testing
    if let Ok(path) = std::env::var("RUNT_TRUST_KEY_PATH") {
        return Some(PathBuf::from(path));
    }
    dirs::config_dir().map(|d| d.join("runt").join("trust-key"))
}

/// Get or create the per-machine trust key.
///
/// The key is stored in `~/.config/runt/trust-key` (or platform equivalent).
/// It's generated randomly on first use and never leaves the machine.
pub fn get_or_create_trust_key() -> Result<[u8; 32], String> {
    let key_path =
        trust_key_path().ok_or_else(|| "Could not determine config directory".to_string())?;

    if key_path.exists() {
        // Read existing key
        let key_bytes =
            std::fs::read(&key_path).map_err(|e| format!("Failed to read trust key: {}", e))?;
        if key_bytes.len() != 32 {
            return Err("Trust key file is corrupted (wrong size)".to_string());
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&key_bytes);
        Ok(key)
    } else {
        // Generate new key
        let key: [u8; 32] = rand::random();

        // Create directory if needed
        if let Some(parent) = key_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config directory: {}", e))?;
        }

        std::fs::write(&key_path, key).map_err(|e| format!("Failed to write trust key: {}", e))?;

        // Restrict key file permissions to owner-only (0600) so other users cannot
        // read the HMAC key and forge trust signatures.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("Failed to set trust key permissions: {}", e))?;
        }

        Ok(key)
    }
}

/// Extract the dependency-related fields from notebook metadata for signing.
///
/// We sign a canonical JSON representation of:
/// - `metadata.runt.uv` (UV dependencies) or `metadata.uv` (legacy)
/// - `metadata.runt.conda` (conda dependencies) or `metadata.conda` (legacy)
///
/// This does NOT include cell contents, outputs, or other metadata.
fn extract_signable_content(metadata: &HashMap<String, serde_json::Value>) -> String {
    let mut signable = serde_json::Map::new();

    // Extract UV deps (check new path first, then legacy)
    if let Some(uv) = get_uv_metadata(metadata) {
        signable.insert("uv".to_string(), uv);
    }

    // Extract conda deps (check new path first, then legacy)
    if let Some(conda) = get_conda_metadata(metadata) {
        signable.insert("conda".to_string(), conda);
    }

    // Extract pixi deps
    if let Some(pixi) = get_pixi_metadata(metadata) {
        signable.insert("pixi".to_string(), pixi);
    }

    // Create canonical JSON (sorted keys)
    serde_json::to_string(&serde_json::Value::Object(signable)).unwrap_or_default()
}

/// Get UV metadata from new path (runt.uv) or legacy path (uv)
pub fn get_uv_metadata(metadata: &HashMap<String, serde_json::Value>) -> Option<serde_json::Value> {
    // New path: metadata.runt.uv
    if let Some(runt) = metadata.get("runt") {
        if let Some(uv) = runt.get("uv") {
            return Some(uv.clone());
        }
    }
    // Legacy path: metadata.uv
    metadata.get("uv").cloned()
}

/// Get conda metadata from new path (runt.conda) or legacy path (conda)
pub fn get_conda_metadata(
    metadata: &HashMap<String, serde_json::Value>,
) -> Option<serde_json::Value> {
    // New path: metadata.runt.conda
    if let Some(runt) = metadata.get("runt") {
        if let Some(conda) = runt.get("conda") {
            return Some(conda.clone());
        }
    }
    // Legacy path: metadata.conda
    metadata.get("conda").cloned()
}

/// Get pixi metadata from runt.pixi
pub fn get_pixi_metadata(
    metadata: &HashMap<String, serde_json::Value>,
) -> Option<serde_json::Value> {
    metadata
        .get("runt")
        .and_then(|runt| runt.get("pixi"))
        .cloned()
}

/// Compute HMAC signature over dependency metadata.
pub fn compute_signature(key: &[u8; 32], metadata: &HashMap<String, serde_json::Value>) -> String {
    let content = extract_signable_content(metadata);

    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC can accept any key size");
    mac.update(content.as_bytes());
    let result = mac.finalize();

    // Encode as hex
    format!("hmac-sha256:{}", hex::encode(result.into_bytes()))
}

/// Verify a signature against the current dependency metadata.
pub fn verify_signature(
    key: &[u8; 32],
    metadata: &HashMap<String, serde_json::Value>,
    signature: &str,
) -> bool {
    // Parse the signature format
    let expected_prefix = "hmac-sha256:";
    if !signature.starts_with(expected_prefix) {
        return false;
    }

    let expected_hex = &signature[expected_prefix.len()..];
    let expected_bytes = match hex::decode(expected_hex) {
        Ok(b) => b,
        Err(_) => return false,
    };

    // Compute current signature
    let content = extract_signable_content(metadata);

    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC can accept any key size");
    mac.update(content.as_bytes());

    // Constant-time comparison
    mac.verify_slice(&expected_bytes).is_ok()
}

/// Check if a notebook has any dependencies configured.
pub fn has_dependencies(metadata: &HashMap<String, serde_json::Value>) -> bool {
    // Check UV dependencies (new path first, then legacy)
    if let Some(uv) = get_uv_metadata(metadata) {
        if let Some(deps) = uv.get("dependencies").and_then(|v| v.as_array()) {
            if !deps.is_empty() {
                return true;
            }
        }
    }

    // Check conda dependencies (new path first, then legacy)
    if let Some(conda) = get_conda_metadata(metadata) {
        if let Some(deps) = conda.get("dependencies").and_then(|v| v.as_array()) {
            if !deps.is_empty() {
                return true;
            }
        }
    }

    // Check pixi dependencies
    if let Some(pixi) = get_pixi_metadata(metadata) {
        if let Some(deps) = pixi.get("dependencies").and_then(|v| v.as_array()) {
            if !deps.is_empty() {
                return true;
            }
        }
        if let Some(deps) = pixi.get("pypi_dependencies").and_then(|v| v.as_array()) {
            if !deps.is_empty() {
                return true;
            }
        }
    }

    false
}

/// Verify the trust status of a notebook.
///
/// Returns the trust status and information about what dependencies would be installed.
pub fn verify_notebook_trust(
    metadata: &HashMap<String, serde_json::Value>,
) -> Result<TrustInfo, String> {
    // Extract dependencies for the response (check new paths first, then legacy)
    let uv_dependencies: Vec<String> = get_uv_metadata(metadata)
        .and_then(|v| v.get("dependencies").cloned())
        .and_then(|v| v.as_array().cloned())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let conda_meta = get_conda_metadata(metadata);

    let conda_dependencies: Vec<String> = conda_meta
        .as_ref()
        .and_then(|v| v.get("dependencies"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let conda_channels: Vec<String> = conda_meta
        .as_ref()
        .and_then(|v| v.get("channels"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // If no dependencies, no trust check needed
    if uv_dependencies.is_empty() && conda_dependencies.is_empty() {
        return Ok(TrustInfo {
            status: TrustStatus::NoDependencies,
            uv_dependencies,
            conda_dependencies,
            conda_channels,
        });
    }

    // Get the trust key
    let key = get_or_create_trust_key()?;

    // Check for existing signature
    let signature = metadata
        .get("runt")
        .and_then(|v| v.get("trust_signature"))
        .and_then(|v| v.as_str());

    let status = match signature {
        None => TrustStatus::Untrusted,
        Some(sig) => {
            if verify_signature(&key, metadata, sig) {
                TrustStatus::Trusted
            } else {
                TrustStatus::SignatureInvalid
            }
        }
    };

    Ok(TrustInfo {
        status,
        uv_dependencies,
        conda_dependencies,
        conda_channels,
    })
}

/// Sign the notebook's dependencies and return the signature.
///
/// The caller is responsible for storing this in `metadata.additional["runt"]["trust_signature"]`.
pub fn sign_notebook_dependencies(
    metadata: &HashMap<String, serde_json::Value>,
) -> Result<String, String> {
    let key = get_or_create_trust_key()?;
    Ok(compute_signature(&key, metadata))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Set up a temporary trust key path for tests.
    /// Returns a guard that cleans up the temp directory when dropped.
    fn setup_test_trust_key() -> tempfile::TempDir {
        let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
        let key_path = temp_dir.path().join("trust-key");
        std::env::set_var("RUNT_TRUST_KEY_PATH", key_path.to_str().unwrap());
        temp_dir
    }

    /// Clean up test trust key path.
    fn teardown_test_trust_key() {
        std::env::remove_var("RUNT_TRUST_KEY_PATH");
    }

    fn make_test_metadata(
        uv_deps: Vec<&str>,
        conda_deps: Vec<&str>,
    ) -> HashMap<String, serde_json::Value> {
        let mut metadata = HashMap::new();

        if !uv_deps.is_empty() {
            metadata.insert(
                "uv".to_string(),
                serde_json::json!({
                    "dependencies": uv_deps,
                }),
            );
        }

        if !conda_deps.is_empty() {
            metadata.insert(
                "conda".to_string(),
                serde_json::json!({
                    "dependencies": conda_deps,
                    "channels": ["conda-forge"],
                }),
            );
        }

        metadata
    }

    #[test]
    fn test_no_dependencies_is_trusted() {
        let metadata = HashMap::new();
        let info = verify_notebook_trust(&metadata).unwrap();
        assert_eq!(info.status, TrustStatus::NoDependencies);
    }

    #[test]
    #[serial]
    fn test_unsigned_notebook_is_untrusted() {
        let _temp = setup_test_trust_key();
        let metadata = make_test_metadata(vec!["pandas"], vec![]);
        let info = verify_notebook_trust(&metadata).unwrap();
        teardown_test_trust_key();
        assert_eq!(info.status, TrustStatus::Untrusted);
    }

    #[test]
    #[serial]
    fn test_sign_and_verify() {
        let _temp = setup_test_trust_key();
        let metadata = make_test_metadata(vec!["pandas", "numpy"], vec![]);

        // Sign the notebook
        let signature = sign_notebook_dependencies(&metadata).unwrap();

        // Add signature to metadata
        let mut signed_metadata = metadata.clone();
        signed_metadata.insert(
            "runt".to_string(),
            serde_json::json!({
                "trust_signature": signature,
            }),
        );

        // Verify it's now trusted
        let info = verify_notebook_trust(&signed_metadata).unwrap();
        teardown_test_trust_key();
        assert_eq!(info.status, TrustStatus::Trusted);
    }

    #[test]
    #[serial]
    fn test_modified_deps_invalidates_signature() {
        let _temp = setup_test_trust_key();
        let metadata = make_test_metadata(vec!["pandas"], vec![]);

        // Sign the notebook
        let signature = sign_notebook_dependencies(&metadata).unwrap();

        // Add signature to metadata
        let mut signed_metadata = metadata;
        signed_metadata.insert(
            "runt".to_string(),
            serde_json::json!({
                "trust_signature": signature,
            }),
        );

        // Modify dependencies (simulate external edit)
        signed_metadata.insert(
            "uv".to_string(),
            serde_json::json!({
                "dependencies": ["pandas", "malicious-pkg"],
            }),
        );

        // Verify signature is now invalid
        let info = verify_notebook_trust(&signed_metadata).unwrap();
        teardown_test_trust_key();
        assert_eq!(info.status, TrustStatus::SignatureInvalid);
    }

    #[test]
    #[serial]
    fn test_signature_format() {
        let _temp = setup_test_trust_key();
        let metadata = make_test_metadata(vec!["pandas"], vec![]);
        let signature = sign_notebook_dependencies(&metadata).unwrap();
        teardown_test_trust_key();
        assert!(signature.starts_with("hmac-sha256:"));
    }

    #[test]
    fn test_trust_info_serialization() {
        // Verify TrustInfo serializes with status as a simple string, not nested object
        let info = TrustInfo {
            status: TrustStatus::NoDependencies,
            uv_dependencies: vec![],
            conda_dependencies: vec![],
            conda_channels: vec![],
        };

        let json = serde_json::to_value(&info).unwrap();

        // status should be a string "no_dependencies", not {"status": "no_dependencies"}
        assert_eq!(json["status"], "no_dependencies");
        assert!(json["status"].is_string());
    }
}
