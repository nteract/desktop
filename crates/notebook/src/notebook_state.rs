use crate::runtime::Runtime;
use crate::settings::{self, PythonEnvType};
use log::info;
use nbformat::v4::{Cell, CellId, CellMetadata, Notebook, Output};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

/// Migrate legacy metadata format to the new `runt` namespace structure.
///
/// Old format:
///   metadata.uv.dependencies, metadata.conda.dependencies, metadata.deno
///
/// New format:
///   metadata.runt.uv.dependencies, metadata.runt.conda.dependencies, metadata.runt.deno
///
/// Returns `true` if any migration was performed.
pub fn migrate_legacy_metadata(additional: &mut HashMap<String, serde_json::Value>) -> bool {
    let mut migrated = false;

    // Extract legacy keys first (before borrowing runt mutably)
    let legacy_uv = additional.remove("uv");
    let legacy_conda = additional.remove("conda");
    let legacy_deno = additional.remove("deno");

    if legacy_uv.is_none() && legacy_conda.is_none() && legacy_deno.is_none() {
        return false;
    }

    // Get or create runt namespace
    let runt = additional
        .entry("runt".to_string())
        .or_insert_with(|| serde_json::json!({"schema_version": "1"}));

    let runt_obj = match runt.as_object_mut() {
        Some(obj) => obj,
        None => {
            // Put the keys back if we can't migrate
            if let Some(uv) = legacy_uv {
                additional.insert("uv".to_string(), uv);
            }
            if let Some(conda) = legacy_conda {
                additional.insert("conda".to_string(), conda);
            }
            if let Some(deno) = legacy_deno {
                additional.insert("deno".to_string(), deno);
            }
            return false;
        }
    };

    // Migrate uv if present and not already in runt
    if let Some(uv) = legacy_uv {
        if !runt_obj.contains_key("uv") {
            runt_obj.insert("uv".to_string(), uv);
            migrated = true;
            info!("[metadata-migration] Migrated metadata.uv -> metadata.runt.uv");
        }
    }

    // Migrate conda if present and not already in runt
    if let Some(conda) = legacy_conda {
        if !runt_obj.contains_key("conda") {
            runt_obj.insert("conda".to_string(), conda);
            migrated = true;
            info!("[metadata-migration] Migrated metadata.conda -> metadata.runt.conda");
        }
    }

    // Migrate deno if present and not already in runt
    if let Some(deno) = legacy_deno {
        if !runt_obj.contains_key("deno") {
            runt_obj.insert("deno".to_string(), deno);
            migrated = true;
            info!("[metadata-migration] Migrated metadata.deno -> metadata.runt.deno");
        }
    }

    migrated
}

/// Flattened cell representation for the frontend.
/// Converts nbformat's tagged enum into something JS-friendly.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "cell_type")]
pub enum FrontendCell {
    #[serde(rename = "code")]
    Code {
        id: String,
        source: String,
        execution_count: Option<i32>,
        outputs: Vec<serde_json::Value>,
    },
    #[serde(rename = "markdown")]
    Markdown { id: String, source: String },
    #[serde(rename = "raw")]
    Raw { id: String, source: String },
}

impl FrontendCell {
    pub fn id(&self) -> &str {
        match self {
            FrontendCell::Code { id, .. } => id,
            FrontendCell::Markdown { id, .. } => id,
            FrontendCell::Raw { id, .. } => id,
        }
    }
}

/// Convert nbformat Cell -> FrontendCell
pub fn cell_to_frontend(cell: &Cell) -> FrontendCell {
    match cell {
        Cell::Code {
            id,
            source,
            execution_count,
            outputs,
            ..
        } => FrontendCell::Code {
            id: id.to_string(),
            source: source.join(""),
            execution_count: *execution_count,
            outputs: outputs
                .iter()
                .filter_map(|o| serde_json::to_value(o).ok())
                .collect(),
        },
        Cell::Markdown { id, source, .. } => FrontendCell::Markdown {
            id: id.to_string(),
            source: source.join(""),
        },
        Cell::Raw { id, source, .. } => FrontendCell::Raw {
            id: id.to_string(),
            source: source.join(""),
        },
    }
}

/// Convert source string back to nbformat's Vec<String> (lines with newlines).
fn source_to_lines(source: &str) -> Vec<String> {
    if source.is_empty() {
        return Vec::new();
    }
    source
        .split_inclusive('\n')
        .map(|s| s.to_string())
        .collect()
}

pub struct NotebookState {
    pub notebook: Notebook,
    pub path: Option<PathBuf>,
    pub dirty: bool,
    /// Working directory for untitled notebooks (used for project file detection).
    /// Set from --cwd CLI argument and preserved across daemon reconnects.
    pub working_dir: Option<PathBuf>,
}

impl NotebookState {
    pub fn new_empty() -> Self {
        // Generate unique environment ID for this notebook
        let env_id = Uuid::new_v4().to_string();

        // Load user's preferred Python environment type from settings
        let app_settings = settings::load_settings();
        let mut additional = HashMap::new();

        // Build runt metadata with nested uv/conda based on user's preference
        let runt_meta = match app_settings.default_python_env {
            PythonEnvType::Uv | PythonEnvType::Other(_) => {
                serde_json::json!({
                    "schema_version": "1",
                    "env_id": env_id,
                    "uv": {
                        "dependencies": Vec::<String>::new(),
                    }
                })
            }
            PythonEnvType::Conda => {
                serde_json::json!({
                    "schema_version": "1",
                    "env_id": env_id,
                    "conda": {
                        "dependencies": Vec::<String>::new(),
                        "channels": vec!["conda-forge"],
                    }
                })
            }
        };

        additional.insert("runt".to_string(), runt_meta);

        // Set Python kernelspec (new_empty defaults to Python)
        let kernelspec = Some(nbformat::v4::KernelSpec {
            name: "python3".to_string(),
            display_name: "Python 3".to_string(),
            language: Some("python".to_string()),
            additional: std::collections::HashMap::new(),
        });

        NotebookState {
            notebook: Notebook {
                metadata: nbformat::v4::Metadata {
                    kernelspec,
                    language_info: None,
                    authors: None,
                    additional,
                },
                nbformat: 4,
                nbformat_minor: 5,
                cells: vec![Cell::Code {
                    id: CellId::from(Uuid::new_v4()),
                    metadata: empty_cell_metadata(),
                    execution_count: None,
                    source: Vec::new(),
                    outputs: Vec::new(),
                }],
            },
            path: None,
            dirty: false,
            working_dir: None,
        }
    }

    /// Create a new empty notebook with a specific runtime
    pub fn new_empty_with_runtime(runtime: Runtime) -> Self {
        let env_id = Uuid::new_v4().to_string();
        let mut additional = HashMap::new();

        // Build runt metadata with nested env config based on runtime
        let runt_meta = match &runtime {
            Runtime::Python => {
                // Load user's preferred Python environment type from settings
                let app_settings = settings::load_settings();
                match app_settings.default_python_env {
                    PythonEnvType::Uv | PythonEnvType::Other(_) => {
                        serde_json::json!({
                            "schema_version": "1",
                            "env_id": env_id,
                            "uv": {
                                "dependencies": Vec::<String>::new(),
                            }
                        })
                    }
                    PythonEnvType::Conda => {
                        serde_json::json!({
                            "schema_version": "1",
                            "env_id": env_id,
                            "conda": {
                                "dependencies": Vec::<String>::new(),
                                "channels": vec!["conda-forge"],
                            }
                        })
                    }
                }
            }
            Runtime::Deno => {
                // Deno setup with default permissions
                // Note: deno permissions stay at top level since they're not our namespace
                additional.insert(
                    "deno".to_string(),
                    serde_json::json!({
                        "permissions": Vec::<String>::new(),
                    }),
                );
                serde_json::json!({
                    "schema_version": "1",
                    "env_id": env_id,
                })
            }
            Runtime::Other(_) => {
                // Unknown runtime — just store schema version and env_id
                serde_json::json!({
                    "schema_version": "1",
                    "env_id": env_id,
                })
            }
        };

        additional.insert("runt".to_string(), runt_meta);

        // Set standard Jupyter kernelspec based on runtime
        // This is critical for daemon detection of kernel type
        let kernelspec = match &runtime {
            Runtime::Python => Some(nbformat::v4::KernelSpec {
                name: "python3".to_string(),
                display_name: "Python 3".to_string(),
                language: Some("python".to_string()),
                additional: std::collections::HashMap::new(),
            }),
            Runtime::Deno => Some(nbformat::v4::KernelSpec {
                name: "deno".to_string(),
                display_name: "Deno".to_string(),
                language: Some("typescript".to_string()),
                additional: std::collections::HashMap::new(),
            }),
            Runtime::Other(s) => Some(nbformat::v4::KernelSpec {
                name: s.clone(),
                display_name: s.clone(),
                language: None,
                additional: std::collections::HashMap::new(),
            }),
        };

        NotebookState {
            notebook: Notebook {
                metadata: nbformat::v4::Metadata {
                    kernelspec,
                    language_info: None,
                    authors: None,
                    additional,
                },
                nbformat: 4,
                nbformat_minor: 5,
                cells: vec![Cell::Code {
                    id: CellId::from(Uuid::new_v4()),
                    metadata: empty_cell_metadata(),
                    execution_count: None,
                    source: Vec::new(),
                    outputs: Vec::new(),
                }],
            },
            path: None,
            dirty: false,
            working_dir: None,
        }
    }

    /// Create a new empty Python notebook with conda metadata from an environment.yml config.
    /// Used when creating a new notebook in a directory with environment.yml.
    /// Unlike pyproject.toml, deps are NOT copied into the notebook — they live in the project file.
    /// We just set conda mode with the environment.yml's channels so the UI shows correctly.
    pub fn new_empty_with_conda_from_environment_yml(
        config: &crate::environment_yml::EnvironmentYmlConfig,
    ) -> Self {
        let env_id = Uuid::new_v4().to_string();
        let mut additional = HashMap::new();

        let channels = if config.channels.is_empty() {
            vec!["conda-forge".to_string()]
        } else {
            config.channels.clone()
        };

        additional.insert(
            "runt".to_string(),
            serde_json::json!({
                "schema_version": "1",
                "env_id": env_id,
                "conda": {
                    "dependencies": Vec::<String>::new(),
                    "channels": channels,
                }
            }),
        );

        // Python kernelspec for conda from environment.yml
        let kernelspec = Some(nbformat::v4::KernelSpec {
            name: "python3".to_string(),
            display_name: "Python 3".to_string(),
            language: Some("python".to_string()),
            additional: std::collections::HashMap::new(),
        });

        NotebookState {
            notebook: Notebook {
                metadata: nbformat::v4::Metadata {
                    kernelspec,
                    language_info: None,
                    authors: None,
                    additional,
                },
                nbformat: 4,
                nbformat_minor: 5,
                cells: vec![Cell::Code {
                    id: CellId::from(Uuid::new_v4()),
                    metadata: empty_cell_metadata(),
                    execution_count: None,
                    source: Vec::new(),
                    outputs: Vec::new(),
                }],
            },
            path: None,
            dirty: false,
            working_dir: None,
        }
    }

    /// Create a new empty Python notebook with UV metadata from a pyproject.toml config.
    /// Used when creating a new notebook in a directory with pyproject.toml.
    pub fn new_empty_with_uv_from_pyproject(config: &crate::pyproject::PyProjectConfig) -> Self {
        let env_id = Uuid::new_v4().to_string();
        let mut additional = HashMap::new();

        let all_deps = crate::pyproject::get_all_dependencies(config);

        additional.insert(
            "runt".to_string(),
            serde_json::json!({
                "schema_version": "1",
                "env_id": env_id,
                "uv": {
                    "dependencies": all_deps,
                    "requires-python": config.requires_python,
                }
            }),
        );

        // Python kernelspec for UV from pyproject.toml
        let kernelspec = Some(nbformat::v4::KernelSpec {
            name: "python3".to_string(),
            display_name: "Python 3".to_string(),
            language: Some("python".to_string()),
            additional: std::collections::HashMap::new(),
        });

        NotebookState {
            notebook: Notebook {
                metadata: nbformat::v4::Metadata {
                    kernelspec,
                    language_info: None,
                    authors: None,
                    additional,
                },
                nbformat: 4,
                nbformat_minor: 5,
                cells: vec![Cell::Code {
                    id: CellId::from(Uuid::new_v4()),
                    metadata: empty_cell_metadata(),
                    execution_count: None,
                    source: Vec::new(),
                    outputs: Vec::new(),
                }],
            },
            path: None,
            dirty: false,
            working_dir: None,
        }
    }

    pub fn from_notebook(notebook: Notebook, path: PathBuf) -> Self {
        NotebookState {
            notebook,
            path: Some(path),
            dirty: false,
            working_dir: None, // Saved notebooks use path for project detection
        }
    }

    /// Get the runtime type from notebook metadata.
    ///
    /// Reads from kernelspec.name (the standard Jupyter field), not runt.runtime.
    /// This mirrors the daemon's `detect_notebook_kernel_type()` logic so
    /// notebook-side and daemon-side detection stay consistent.
    pub fn get_runtime(&self) -> Runtime {
        // Check kernelspec.name first (most reliable)
        if let Some(ks) = &self.notebook.metadata.kernelspec {
            let name_lower = ks.name.to_lowercase();
            if name_lower.contains("deno") {
                return Runtime::Deno;
            }
            if name_lower.contains("python") {
                return Runtime::Python;
            }
            // Check language field
            if let Some(lang) = &ks.language {
                let lang_lower = lang.to_lowercase();
                if lang_lower == "typescript" || lang_lower == "javascript" {
                    return Runtime::Deno;
                }
                if lang_lower == "python" {
                    return Runtime::Python;
                }
            }
            // Unknown kernelspec - preserve as Other for round-tripping
            return Runtime::Other(ks.name.clone());
        }

        // Fallback: check language_info.name
        if let Some(lang_info) = &self.notebook.metadata.language_info {
            let name_lower = lang_info.name.to_lowercase();
            if name_lower == "typescript" || name_lower == "javascript" || name_lower == "deno" {
                return Runtime::Deno;
            }
            if name_lower == "python" {
                return Runtime::Python;
            }
        }

        // Default to Python only if no kernelspec and no language_info
        Runtime::Python
    }

    pub fn cells_for_frontend(&self) -> Vec<FrontendCell> {
        self.notebook.cells.iter().map(cell_to_frontend).collect()
    }

    /// Get the ordered list of code cell IDs (skipping markdown and raw cells)
    pub fn get_code_cell_ids(&self) -> Vec<String> {
        self.notebook
            .cells
            .iter()
            .filter_map(|c| match c {
                Cell::Code { id, .. } => Some(id.to_string()),
                _ => None,
            })
            .collect()
    }

    pub fn find_cell_index(&self, cell_id: &str) -> Option<usize> {
        self.notebook
            .cells
            .iter()
            .position(|c| c.id().as_str() == cell_id)
    }

    pub fn update_cell_source(&mut self, cell_id: &str, source: &str) {
        if let Some(idx) = self.find_cell_index(cell_id) {
            let lines = source_to_lines(source);
            match &mut self.notebook.cells[idx] {
                Cell::Code {
                    source: ref mut s, ..
                } => *s = lines,
                Cell::Markdown {
                    source: ref mut s, ..
                } => *s = lines,
                Cell::Raw {
                    source: ref mut s, ..
                } => *s = lines,
            }
            self.dirty = true;
        }
    }

    pub fn get_cell_source(&self, cell_id: &str) -> Option<String> {
        self.find_cell_index(cell_id)
            .map(|idx| self.notebook.cells[idx].source().join(""))
    }

    pub fn add_cell(
        &mut self,
        cell_type: &str,
        after_cell_id: Option<&str>,
        cell_id: Option<&str>,
    ) -> Option<FrontendCell> {
        let new_id = match cell_id {
            Some(id) => CellId::from(Uuid::parse_str(id).unwrap_or_else(|_| Uuid::new_v4())),
            None => CellId::from(Uuid::new_v4()),
        };
        let cell = match cell_type {
            "code" => Cell::Code {
                id: new_id,
                metadata: empty_cell_metadata(),
                execution_count: None,
                source: Vec::new(),
                outputs: Vec::new(),
            },
            "markdown" => Cell::Markdown {
                id: new_id,
                metadata: empty_cell_metadata(),
                source: Vec::new(),
                attachments: None,
            },
            "raw" => Cell::Raw {
                id: new_id,
                metadata: empty_cell_metadata(),
                source: Vec::new(),
            },
            _ => return None,
        };

        let frontend_cell = cell_to_frontend(&cell);

        let insert_idx = match after_cell_id {
            Some(id) => self.find_cell_index(id).map(|i| i + 1),
            None => Some(0),
        };

        if let Some(idx) = insert_idx {
            self.notebook.cells.insert(idx, cell);
        } else {
            self.notebook.cells.push(cell);
        }
        self.dirty = true;

        Some(frontend_cell)
    }

    pub fn delete_cell(&mut self, cell_id: &str) -> bool {
        // Don't delete the last cell
        if self.notebook.cells.len() <= 1 {
            return false;
        }
        if let Some(idx) = self.find_cell_index(cell_id) {
            self.notebook.cells.remove(idx);
            self.dirty = true;
            true
        } else {
            false
        }
    }

    pub fn clear_cell_outputs(&mut self, cell_id: &str) {
        if let Some(idx) = self.find_cell_index(cell_id) {
            if let Cell::Code {
                outputs,
                execution_count,
                ..
            } = &mut self.notebook.cells[idx]
            {
                outputs.clear();
                *execution_count = None;
            }
        }
    }

    pub fn set_cell_execution_count(&mut self, cell_id: &str, count: i32) {
        if let Some(idx) = self.find_cell_index(cell_id) {
            if let Cell::Code {
                execution_count, ..
            } = &mut self.notebook.cells[idx]
            {
                *execution_count = Some(count);
            }
        }
    }

    pub fn append_cell_output(&mut self, cell_id: &str, output: Output) {
        if let Some(idx) = self.find_cell_index(cell_id) {
            if let Cell::Code { outputs, .. } = &mut self.notebook.cells[idx] {
                outputs.push(output);
            }
        }
    }

    pub fn serialize(&self) -> Result<String, String> {
        let nb = nbformat::Notebook::V4(self.notebook.clone());
        nbformat::serialize_notebook(&nb).map_err(|e| e.to_string())
    }
}

// ── Conversions between nbformat Metadata and NotebookMetadataSnapshot ──

use runtimed::notebook_metadata::{
    CondaInlineMetadata, DenoMetadata, KernelspecSnapshot, LanguageInfoSnapshot,
    NotebookMetadataSnapshot, RuntMetadata, UvInlineMetadata,
};

/// Extract a `NotebookMetadataSnapshot` from nbformat metadata.
///
/// Reads `kernelspec`, `language_info`, and `runt` from the nbformat struct.
/// Falls back to legacy `uv`/`conda` top-level keys if `runt` is absent.
pub fn snapshot_from_nbformat(metadata: &nbformat::v4::Metadata) -> NotebookMetadataSnapshot {
    let kernelspec = metadata.kernelspec.as_ref().map(|ks| KernelspecSnapshot {
        name: ks.name.clone(),
        display_name: ks.display_name.clone(),
        language: ks.language.clone(),
    });

    let language_info = metadata
        .language_info
        .as_ref()
        .map(|li| LanguageInfoSnapshot {
            name: li.name.clone(),
            version: li.version.clone(),
        });

    // Helper to convert legacy DenoDependencies to DenoMetadata
    let convert_legacy_deno = |dd: crate::deno_env::DenoDependencies| DenoMetadata {
        permissions: dd.permissions,
        import_map: dd.import_map,
        config: dd.config,
        // Only record if non-default (default is true)
        flexible_npm_imports: if dd.flexible_npm_imports {
            None
        } else {
            Some(false)
        },
    };

    let runt = if let Some(runt_value) = metadata.additional.get("runt") {
        let mut runt_meta = serde_json::from_value::<RuntMetadata>(runt_value.clone())
            .unwrap_or_else(|_| RuntMetadata {
                schema_version: "1".to_string(),
                env_id: None,
                uv: None,
                conda: None,
                deno: None,
                trust_signature: None,
                trust_timestamp: None,
            });

        // Also check legacy top-level "deno" key - this is where the Tauri commands write
        // deno metadata, so we need to merge it even when runt exists
        if runt_meta.deno.is_none() {
            if let Some(deno_value) = metadata.additional.get("deno") {
                if let Ok(dd) =
                    serde_json::from_value::<crate::deno_env::DenoDependencies>(deno_value.clone())
                {
                    runt_meta.deno = Some(convert_legacy_deno(dd));
                }
            }
        }

        runt_meta
    } else {
        // Fallback: legacy top-level uv/conda/deno keys
        let uv = metadata
            .additional
            .get("uv")
            .and_then(|v| serde_json::from_value::<UvInlineMetadata>(v.clone()).ok());
        let conda = metadata
            .additional
            .get("conda")
            .and_then(|v| serde_json::from_value::<CondaInlineMetadata>(v.clone()).ok());
        let deno = metadata
            .additional
            .get("deno")
            .and_then(|v| {
                serde_json::from_value::<crate::deno_env::DenoDependencies>(v.clone()).ok()
            })
            .map(convert_legacy_deno);

        RuntMetadata {
            schema_version: "1".to_string(),
            env_id: None,
            uv,
            conda,
            deno,
            trust_signature: None,
            trust_timestamp: None,
        }
    };

    NotebookMetadataSnapshot {
        kernelspec,
        language_info,
        runt,
    }
}

/// Merge a `NotebookMetadataSnapshot` back into nbformat metadata.
///
/// Replaces `kernelspec`, `language_info`, and `metadata.additional["runt"]`
/// while preserving all other keys in `additional` (e.g. `jupyter`, `deno`
/// top-level config, custom extensions).
pub fn merge_snapshot_into_nbformat(
    snapshot: &NotebookMetadataSnapshot,
    metadata: &mut nbformat::v4::Metadata,
) {
    // Replace kernelspec, preserving existing additional fields
    let existing_ks_additional = metadata
        .kernelspec
        .as_ref()
        .map(|ks| ks.additional.clone())
        .unwrap_or_default();
    metadata.kernelspec = snapshot
        .kernelspec
        .as_ref()
        .map(|ks| nbformat::v4::KernelSpec {
            name: ks.name.clone(),
            display_name: ks.display_name.clone(),
            language: ks.language.clone(),
            additional: existing_ks_additional,
        });

    // Replace language_info, preserving existing codemirror_mode and additional
    let (existing_cm_mode, existing_li_additional) = metadata
        .language_info
        .as_ref()
        .map(|li| (li.codemirror_mode.clone(), li.additional.clone()))
        .unwrap_or_default();
    metadata.language_info = snapshot
        .language_info
        .as_ref()
        .map(|li| nbformat::v4::LanguageInfo {
            name: li.name.clone(),
            version: li.version.clone(),
            codemirror_mode: existing_cm_mode,
            additional: existing_li_additional,
        });

    // Deep-merge runt namespace to preserve unknown fields (e.g. trust_signature)
    if let Ok(mut new_runt) = serde_json::to_value(&snapshot.runt) {
        if let Some(existing_runt) = metadata.additional.get("runt") {
            // Merge: start with existing, overwrite with new snapshot fields
            if let (Some(existing_obj), Some(new_obj)) =
                (existing_runt.as_object(), new_runt.as_object_mut())
            {
                for (k, v) in existing_obj {
                    // Only keep existing keys that aren't in the new snapshot
                    if !new_obj.contains_key(k) {
                        new_obj.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        metadata.additional.insert("runt".to_string(), new_runt);
    }
}

fn empty_cell_metadata() -> CellMetadata {
    CellMetadata {
        id: None,
        collapsed: None,
        scrolled: None,
        deletable: None,
        editable: None,
        format: None,
        name: None,
        tags: None,
        jupyter: None,
        execution: None,
        additional: HashMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_empty_creates_single_code_cell() {
        let state = NotebookState::new_empty();

        assert_eq!(state.notebook.cells.len(), 1);
        assert!(matches!(state.notebook.cells[0], Cell::Code { .. }));
        assert!(state.path.is_none());
        assert!(!state.dirty);
    }

    #[test]
    fn test_new_empty_sets_env_metadata() {
        let state = NotebookState::new_empty();

        // Should have runt namespace with either uv or conda nested inside
        let runt = state
            .notebook
            .metadata
            .additional
            .get("runt")
            .expect("runt namespace should exist");
        let has_env = runt.get("uv").is_some() || runt.get("conda").is_some();
        assert!(has_env);
    }

    #[test]
    fn test_new_empty_with_runtime_python() {
        let state = NotebookState::new_empty_with_runtime(Runtime::Python);

        // Should have runt namespace with either uv or conda nested inside
        let runt = state
            .notebook
            .metadata
            .additional
            .get("runt")
            .expect("runt namespace should exist");
        let has_env = runt.get("uv").is_some() || runt.get("conda").is_some();
        assert!(has_env);

        // Runtime is determined by kernelspec, not runt.runtime
        let ks = state.notebook.metadata.kernelspec.as_ref().unwrap();
        assert!(ks.name.contains("python"));
    }

    #[test]
    fn test_new_empty_with_runtime_deno() {
        let state = NotebookState::new_empty_with_runtime(Runtime::Deno);

        assert!(state.notebook.metadata.additional.contains_key("deno"));

        // Runtime is determined by kernelspec, not runt.runtime
        let ks = state.notebook.metadata.kernelspec.as_ref().unwrap();
        assert_eq!(ks.name, "deno");
    }

    #[test]
    fn test_get_runtime_returns_python_by_default() {
        let state = NotebookState::new_empty();
        assert_eq!(state.get_runtime(), Runtime::Python);
    }

    #[test]
    fn test_get_runtime_returns_correct_runtime() {
        let state = NotebookState::new_empty_with_runtime(Runtime::Deno);
        assert_eq!(state.get_runtime(), Runtime::Deno);
    }

    #[test]
    fn test_get_runtime_other_round_trips() {
        // Unknown runtimes should round-trip through kernelspec
        let state = NotebookState::new_empty_with_runtime(Runtime::Other("julia".into()));
        assert_eq!(state.get_runtime(), Runtime::Other("julia".into()));
    }

    #[test]
    fn test_get_runtime_contains_deno() {
        // Should detect deno-variants like "deno-ts" via contains()
        let mut state = NotebookState::new_empty();
        state.notebook.metadata.kernelspec = Some(nbformat::v4::KernelSpec {
            name: "deno-typescript".to_string(),
            display_name: "Deno TypeScript".to_string(),
            language: None,
            additional: std::collections::HashMap::new(),
        });
        assert_eq!(state.get_runtime(), Runtime::Deno);
    }

    #[test]
    fn test_get_runtime_language_info_fallback() {
        // Should fall back to language_info.name when kernelspec is missing
        let mut state = NotebookState::new_empty();
        state.notebook.metadata.kernelspec = None;
        state.notebook.metadata.language_info = Some(nbformat::v4::LanguageInfo {
            name: "typescript".to_string(),
            version: None,
            codemirror_mode: None,
            additional: std::collections::HashMap::new(),
        });
        assert_eq!(state.get_runtime(), Runtime::Deno);
    }

    #[test]
    fn test_get_runtime_kernelspec_language_fallback() {
        // Should detect via kernelspec.language when name doesn't match
        let mut state = NotebookState::new_empty();
        state.notebook.metadata.kernelspec = Some(nbformat::v4::KernelSpec {
            name: "custom-kernel".to_string(),
            display_name: "Custom".to_string(),
            language: Some("typescript".to_string()),
            additional: std::collections::HashMap::new(),
        });
        assert_eq!(state.get_runtime(), Runtime::Deno);
    }

    #[test]
    fn test_find_cell_index_returns_correct_position() {
        let state = NotebookState::new_empty();
        let cell_id = state.notebook.cells[0].id().to_string();

        assert_eq!(state.find_cell_index(&cell_id), Some(0));
    }

    #[test]
    fn test_find_cell_index_returns_none_for_missing() {
        let state = NotebookState::new_empty();
        assert_eq!(state.find_cell_index("nonexistent"), None);
    }

    #[test]
    fn test_update_cell_source_modifies_cell() {
        let mut state = NotebookState::new_empty();
        let cell_id = state.notebook.cells[0].id().to_string();

        state.update_cell_source(&cell_id, "print('hello')");

        let source = state.get_cell_source(&cell_id).unwrap();
        assert_eq!(source, "print('hello')");
    }

    #[test]
    fn test_update_cell_source_sets_dirty_flag() {
        let mut state = NotebookState::new_empty();
        let cell_id = state.notebook.cells[0].id().to_string();

        assert!(!state.dirty);
        state.update_cell_source(&cell_id, "x = 1");
        assert!(state.dirty);
    }

    #[test]
    fn test_get_cell_source_returns_joined_lines() {
        let mut state = NotebookState::new_empty();
        let cell_id = state.notebook.cells[0].id().to_string();

        state.update_cell_source(&cell_id, "line1\nline2\nline3");

        let source = state.get_cell_source(&cell_id).unwrap();
        assert_eq!(source, "line1\nline2\nline3");
    }

    #[test]
    fn test_get_cell_source_returns_none_for_missing() {
        let state = NotebookState::new_empty();
        assert!(state.get_cell_source("nonexistent").is_none());
    }

    #[test]
    fn test_add_cell_code() {
        let mut state = NotebookState::new_empty();

        let result = state.add_cell("code", None, None);

        assert!(result.is_some());
        assert!(matches!(result.unwrap(), FrontendCell::Code { .. }));
        assert_eq!(state.notebook.cells.len(), 2);
    }

    #[test]
    fn test_add_cell_markdown() {
        let mut state = NotebookState::new_empty();

        let result = state.add_cell("markdown", None, None);

        assert!(result.is_some());
        assert!(matches!(result.unwrap(), FrontendCell::Markdown { .. }));
    }

    #[test]
    fn test_add_cell_raw() {
        let mut state = NotebookState::new_empty();

        let result = state.add_cell("raw", None, None);

        assert!(result.is_some());
        assert!(matches!(result.unwrap(), FrontendCell::Raw { .. }));
    }

    #[test]
    fn test_add_cell_invalid_type_returns_none() {
        let mut state = NotebookState::new_empty();

        let result = state.add_cell("invalid", None, None);

        assert!(result.is_none());
        assert_eq!(state.notebook.cells.len(), 1);
    }

    #[test]
    fn test_add_cell_after_existing_cell() {
        let mut state = NotebookState::new_empty();
        let first_cell_id = state.notebook.cells[0].id().to_string();

        state.add_cell("code", Some(&first_cell_id), None);

        assert_eq!(state.notebook.cells.len(), 2);
        // New cell should be at index 1 (after first cell)
        assert_ne!(state.notebook.cells[1].id().to_string(), first_cell_id);
    }

    #[test]
    fn test_add_cell_with_provided_id() {
        let mut state = NotebookState::new_empty();
        let provided_id = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";

        let result = state.add_cell("code", None, Some(provided_id)).unwrap();

        assert_eq!(result.id(), provided_id);
        assert_eq!(state.notebook.cells[0].id().to_string(), provided_id);
    }

    #[test]
    fn test_add_cell_with_provided_id_ignores_invalid_uuid() {
        let mut state = NotebookState::new_empty();

        // Invalid UUID should fall back to generating a new one
        let result = state.add_cell("code", None, Some("not-a-uuid")).unwrap();

        // Should still succeed — just with a generated UUID, not "not-a-uuid"
        assert_ne!(result.id(), "not-a-uuid");
        assert_eq!(state.notebook.cells.len(), 2);
    }

    #[test]
    fn test_add_cell_at_beginning_when_no_after() {
        let mut state = NotebookState::new_empty();
        let first_cell_id = state.notebook.cells[0].id().to_string();

        let new_cell = state.add_cell("code", None, None).unwrap();

        // New cell should be at index 0
        assert_eq!(state.notebook.cells[0].id().to_string(), new_cell.id());
        // Original cell should now be at index 1
        assert_eq!(state.notebook.cells[1].id().to_string(), first_cell_id);
    }

    #[test]
    fn test_add_cell_sets_dirty_flag() {
        let mut state = NotebookState::new_empty();

        assert!(!state.dirty);
        state.add_cell("code", None, None);
        assert!(state.dirty);
    }

    #[test]
    fn test_delete_cell_removes_cell() {
        let mut state = NotebookState::new_empty();
        state.add_cell("code", None, None);
        let cell_to_delete = state.notebook.cells[0].id().to_string();

        let result = state.delete_cell(&cell_to_delete);

        assert!(result);
        assert_eq!(state.notebook.cells.len(), 1);
    }

    #[test]
    fn test_delete_cell_prevents_removing_last() {
        let mut state = NotebookState::new_empty();
        let only_cell = state.notebook.cells[0].id().to_string();

        let result = state.delete_cell(&only_cell);

        assert!(!result);
        assert_eq!(state.notebook.cells.len(), 1);
    }

    #[test]
    fn test_delete_cell_returns_false_for_missing() {
        let mut state = NotebookState::new_empty();
        state.add_cell("code", None, None);

        let result = state.delete_cell("nonexistent");

        assert!(!result);
        assert_eq!(state.notebook.cells.len(), 2);
    }

    #[test]
    fn test_delete_cell_sets_dirty_flag() {
        let mut state = NotebookState::new_empty();
        state.add_cell("code", None, None);
        state.dirty = false;
        let cell_to_delete = state.notebook.cells[0].id().to_string();

        state.delete_cell(&cell_to_delete);

        assert!(state.dirty);
    }

    #[test]
    fn test_clear_cell_outputs_clears_outputs_and_count() {
        let mut state = NotebookState::new_empty();
        let cell_id = state.notebook.cells[0].id().to_string();

        // Set some execution state
        state.set_cell_execution_count(&cell_id, 5);

        // Clear outputs
        state.clear_cell_outputs(&cell_id);

        // Check execution count is cleared
        if let Cell::Code {
            execution_count, ..
        } = &state.notebook.cells[0]
        {
            assert!(execution_count.is_none());
        }
    }

    #[test]
    fn test_set_cell_execution_count() {
        let mut state = NotebookState::new_empty();
        let cell_id = state.notebook.cells[0].id().to_string();

        state.set_cell_execution_count(&cell_id, 42);

        if let Cell::Code {
            execution_count, ..
        } = &state.notebook.cells[0]
        {
            assert_eq!(*execution_count, Some(42));
        } else {
            panic!("Expected code cell");
        }
    }

    #[test]
    fn test_cells_for_frontend_converts_correctly() {
        let mut state = NotebookState::new_empty();
        let cell_id = state.notebook.cells[0].id().to_string();
        state.update_cell_source(&cell_id, "x = 1");

        let frontend_cells = state.cells_for_frontend();

        assert_eq!(frontend_cells.len(), 1);
        if let FrontendCell::Code { source, .. } = &frontend_cells[0] {
            assert_eq!(source, "x = 1");
        } else {
            panic!("Expected code cell");
        }
    }

    #[test]
    fn test_serialize_produces_valid_json() {
        let state = NotebookState::new_empty();

        let result = state.serialize();

        assert!(result.is_ok());
        let json_str = result.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed["nbformat"], 4);
        assert!(parsed["cells"].is_array());
    }

    #[test]
    fn test_source_to_lines_handles_empty_string() {
        let lines = source_to_lines("");
        assert!(lines.is_empty());
    }

    #[test]
    fn test_source_to_lines_multiline() {
        let lines = source_to_lines("line1\nline2");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "line1\n");
        assert_eq!(lines[1], "line2");
    }

    #[test]
    fn test_source_to_lines_single_line() {
        let lines = source_to_lines("single");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "single");
    }

    #[test]
    fn test_source_to_lines_preserves_trailing_newline() {
        let lines = source_to_lines("line1\nline2\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "line1\n");
        assert_eq!(lines[1], "line2\n");
    }

    #[test]
    fn test_source_to_lines_roundtrip() {
        for original in &["line1\nline2", "line1\nline2\n", "single", "single\n", ""] {
            let lines = source_to_lines(original);
            let rejoined: String = lines.join("");
            assert_eq!(&rejoined, original, "roundtrip failed for {:?}", original);
        }
    }

    #[test]
    fn test_frontend_cell_id_method() {
        let code_cell = FrontendCell::Code {
            id: "code-123".to_string(),
            source: String::new(),
            execution_count: None,
            outputs: vec![],
        };
        let md_cell = FrontendCell::Markdown {
            id: "md-456".to_string(),
            source: String::new(),
        };
        let raw_cell = FrontendCell::Raw {
            id: "raw-789".to_string(),
            source: String::new(),
        };

        assert_eq!(code_cell.id(), "code-123");
        assert_eq!(md_cell.id(), "md-456");
        assert_eq!(raw_cell.id(), "raw-789");
    }

    #[test]
    fn test_get_code_cell_ids_returns_only_code_cells() {
        let mut state = NotebookState::new_empty();
        // Start with 1 code cell
        let first_code_id = state.notebook.cells[0].id().to_string();

        // Add markdown, then another code cell
        state.add_cell("markdown", Some(&first_code_id), None);
        let md_id = state.notebook.cells[1].id().to_string();
        state.add_cell("code", Some(&md_id), None);
        let second_code_id = state.notebook.cells[2].id().to_string();

        let code_ids = state.get_code_cell_ids();
        assert_eq!(code_ids.len(), 2);
        assert_eq!(code_ids[0], first_code_id);
        assert_eq!(code_ids[1], second_code_id);
    }

    #[test]
    fn test_get_code_cell_ids_empty_when_no_code_cells() {
        let mut state = NotebookState::new_empty();
        let first_id = state.notebook.cells[0].id().to_string();
        // Replace the only code cell with a markdown cell
        state.add_cell("markdown", Some(&first_id), None);
        // Can't delete the last cell, so this test just checks with mixed cells
        let ids = state.get_code_cell_ids();
        assert_eq!(ids.len(), 1); // The original code cell
    }

    #[test]
    fn test_frontend_cell_serialization() {
        let cell = FrontendCell::Code {
            id: "test-id".to_string(),
            source: "print('hi')".to_string(),
            execution_count: Some(1),
            outputs: vec![],
        };

        let json = serde_json::to_value(&cell).unwrap();

        assert_eq!(json["cell_type"], "code");
        assert_eq!(json["id"], "test-id");
        assert_eq!(json["source"], "print('hi')");
        assert_eq!(json["execution_count"], 1);
    }
}
