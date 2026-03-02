//! Automerge-backed settings document for cross-window sync.
//!
//! Wraps an Automerge `AutoCommit` document with typed accessors for
//! application settings. The daemon holds the canonical copy; each connected
//! notebook window holds a local replica that syncs over the Automerge sync
//! protocol.
//!
//! The document uses nested maps for environment-specific settings:
//!
//! ```text
//! ROOT/
//!   theme: "system"
//!   default_runtime: "python"
//!   default_python_env: "uv"
//!   uv/                           ← nested Map
//!     default_packages: List[…]   ← List of Str
//!   conda/                        ← nested Map
//!     default_packages: List[…]   ← List of Str
//! ```

use std::path::Path;

use automerge::sync;
use automerge::sync::SyncDoc;
use automerge::transaction::Transactable;
use automerge::{AutoCommit, AutomergeError, ObjId, ObjType, ReadDoc};
use log::info;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// UI theme mode for the notebook editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum ThemeMode {
    /// Follow the OS preference and update automatically
    #[default]
    System,
    /// Force light mode
    Light,
    /// Force dark mode
    Dark,
}

impl std::fmt::Display for ThemeMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThemeMode::System => write!(f, "system"),
            ThemeMode::Light => write!(f, "light"),
            ThemeMode::Dark => write!(f, "dark"),
        }
    }
}

use crate::runtime::Runtime;

/// Python environment type for dependency management.
///
/// Unknown values are captured in the `Other` variant so they survive
/// serialization round-trips across branches that add new env types.
#[derive(Debug, Clone, PartialEq, Eq, Default, TS)]
#[ts(export)]
#[ts(type = "\"uv\" | \"conda\" | (string & {})")]
pub enum PythonEnvType {
    /// Use uv for Python package management (fast, pip-compatible)
    #[default]
    Uv,
    /// Use conda/rattler for Python package management (supports conda packages)
    Conda,
    /// An unrecognized env type value, preserved for round-tripping.
    Other(String),
}

impl serde::Serialize for PythonEnvType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for PythonEnvType {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(s.parse().expect("FromStr for PythonEnvType is infallible"))
    }
}

impl JsonSchema for PythonEnvType {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "PythonEnvType".into()
    }

    fn json_schema(_gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "examples": ["uv", "conda"]
        })
    }
}

impl std::fmt::Display for PythonEnvType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PythonEnvType::Uv => write!(f, "uv"),
            PythonEnvType::Conda => write!(f, "conda"),
            PythonEnvType::Other(s) => write!(f, "{}", s),
        }
    }
}

impl std::str::FromStr for PythonEnvType {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "uv" => PythonEnvType::Uv,
            "conda" => PythonEnvType::Conda,
            _ => PythonEnvType::Other(s.to_string()),
        })
    }
}

/// Default packages for uv environments.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[ts(export)]
pub struct UvDefaults {
    pub default_packages: Vec<String>,
}

/// Default packages for conda environments.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[ts(export)]
pub struct CondaDefaults {
    pub default_packages: Vec<String>,
}

/// Default keep-alive duration in seconds for notebook rooms.
/// When all clients disconnect, the daemon waits this long before evicting the room.
pub const DEFAULT_KEEP_ALIVE_SECS: u64 = 30;

/// Minimum keep-alive duration (5 seconds) to prevent accidental instant eviction.
pub const MIN_KEEP_ALIVE_SECS: u64 = 5;

/// Maximum keep-alive duration (7 days) for notebook rooms.
pub const MAX_KEEP_ALIVE_SECS: u64 = 604800;

/// Snapshot of all synced settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[ts(export)]
pub struct SyncedSettings {
    /// UI theme
    #[serde(default)]
    pub theme: ThemeMode,

    /// Default runtime for new notebooks
    #[serde(default)]
    pub default_runtime: Runtime,

    /// Default Python environment type (uv or conda)
    #[serde(default)]
    pub default_python_env: PythonEnvType,

    /// UV environment defaults
    #[serde(default)]
    pub uv: UvDefaults,

    /// Conda environment defaults
    #[serde(default)]
    pub conda: CondaDefaults,

    /// How long (in seconds) to keep notebook rooms alive after all clients disconnect.
    /// This allows you to close and reopen the window without losing your kernel state.
    /// Range: 5 seconds to 7 days (604800 seconds).
    #[serde(default = "default_keep_alive_secs")]
    pub keep_alive_secs: u64,
}

impl Default for SyncedSettings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::default(),
            default_runtime: Runtime::default(),
            default_python_env: PythonEnvType::default(),
            uv: UvDefaults::default(),
            conda: CondaDefaults::default(),
            keep_alive_secs: DEFAULT_KEEP_ALIVE_SECS,
        }
    }
}

fn default_keep_alive_secs() -> u64 {
    DEFAULT_KEEP_ALIVE_SECS
}

/// Generate a JSON Schema string for the settings file.
pub fn generate_settings_schema() -> Result<String, serde_json::Error> {
    let schema = schemars::schema_for!(SyncedSettings);
    serde_json::to_string_pretty(&schema)
}

/// Write the settings schema file to disk.
pub fn write_settings_schema() -> std::io::Result<()> {
    let path = crate::settings_schema_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let schema = generate_settings_schema().map_err(std::io::Error::other)?;
    std::fs::write(&path, format!("{schema}\n"))
}

/// Wrapper around an Automerge document storing application settings.
///
/// The document uses a mix of root-level scalar strings and nested maps
/// containing lists for environment-specific settings.
pub struct SettingsDoc {
    doc: AutoCommit,
}

impl SettingsDoc {
    /// Create a new empty settings document with defaults.
    pub fn new() -> Self {
        let mut doc = AutoCommit::new();
        let defaults = SyncedSettings::default();

        // Root-level scalars (Automerge stores strings; enums are serialized via Display)
        let _ = doc.put(automerge::ROOT, "theme", defaults.theme.to_string());
        let _ = doc.put(
            automerge::ROOT,
            "default_runtime",
            defaults.default_runtime.to_string(),
        );
        let _ = doc.put(
            automerge::ROOT,
            "default_python_env",
            defaults.default_python_env.to_string(),
        );
        // Store keep_alive_secs as i64 (Automerge's numeric type)
        let _ = doc.put(
            automerge::ROOT,
            "keep_alive_secs",
            defaults.keep_alive_secs as i64,
        );

        // Nested uv map with empty package list
        if let Ok(uv_id) = doc.put_object(automerge::ROOT, "uv", ObjType::Map) {
            let _ = doc.put_object(&uv_id, "default_packages", ObjType::List);
        }

        // Nested conda map with empty package list
        if let Ok(conda_id) = doc.put_object(automerge::ROOT, "conda", ObjType::Map) {
            let _ = doc.put_object(&conda_id, "default_packages", ObjType::List);
        }

        Self { doc }
    }

    /// Load a settings document from a saved binary, or create a new one with
    /// defaults if the file doesn't exist or is invalid.
    ///
    /// If `settings_json_path` points to an existing `settings.json`, its values
    /// are migrated into the new Automerge document.
    ///
    /// Existing Automerge docs with old flat keys (`default_uv_packages`,
    /// `default_conda_packages`) are migrated to the nested structure on load.
    pub fn load_or_create(automerge_path: &Path, settings_json_path: Option<&Path>) -> Self {
        // Try loading existing Automerge document
        if automerge_path.exists() {
            if let Ok(data) = std::fs::read(automerge_path) {
                if let Ok(doc) = AutoCommit::load(&data) {
                    info!("[settings] Loaded Automerge doc from {:?}", automerge_path);
                    let mut settings = Self { doc };
                    settings.migrate_flat_to_nested();
                    settings.migrate_null_keep_alive();

                    // Reconcile with settings.json so manual edits made while the
                    // daemon was stopped are picked up (the file watcher only
                    // catches changes that happen after it starts).
                    if let Some(json_path) = settings_json_path {
                        if json_path.exists() {
                            if let Ok(contents) = std::fs::read_to_string(json_path) {
                                if let Ok(json) = serde_json::from_str(&contents) {
                                    if settings.apply_json_changes(&json) {
                                        info!("[settings] Reconciled Automerge doc with settings.json");
                                    }
                                }
                            }
                        }
                    }

                    return settings;
                }
            }
        }

        // Try migrating from settings.json
        if let Some(json_path) = settings_json_path {
            if json_path.exists() {
                if let Ok(contents) = std::fs::read_to_string(json_path) {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) {
                        info!("[settings] Migrating from {:?}", json_path);
                        return Self::from_json(&json);
                    }
                }
            }
        }

        // Fall back to defaults
        info!("[settings] Creating new settings doc with defaults");
        Self::new()
    }

    /// Create a settings document from parsed JSON (for migration from settings.json).
    fn from_json(json: &serde_json::Value) -> Self {
        let mut settings = Self::new();

        if let Some(theme) = json.get("theme").and_then(|v| v.as_str()) {
            settings.put("theme", theme);
        }
        if let Some(runtime) = json.get("default_runtime").and_then(|v| v.as_str()) {
            settings.put("default_runtime", runtime);
        }
        if let Some(env) = json.get("default_python_env").and_then(|v| v.as_str()) {
            settings.put("default_python_env", env);
        }
        // keep_alive_secs: numeric value in seconds (5 to 604800)
        // Legacy null means "forever", which we migrate to MAX_KEEP_ALIVE_SECS
        if let Some(val) = json.get("keep_alive_secs") {
            if val.is_null() {
                // Migration: null (legacy "forever" mode) -> MAX_KEEP_ALIVE_SECS
                settings.put_u64("keep_alive_secs", MAX_KEEP_ALIVE_SECS);
            } else if let Some(secs) = val.as_u64() {
                settings.put_u64("keep_alive_secs", secs);
            }
        }

        let uv_packages = Self::extract_packages_from_json(json, "uv");
        if !uv_packages.is_empty() {
            settings.put_list("uv.default_packages", &uv_packages);
        }

        let conda_packages = Self::extract_packages_from_json(json, "conda");
        if !conda_packages.is_empty() {
            settings.put_list("conda.default_packages", &conda_packages);
        }

        settings
    }

    /// Extract packages from a nested JSON key (e.g. `uv.default_packages`).
    fn extract_packages_from_json(json: &serde_json::Value, nested_key: &str) -> Vec<String> {
        if let Some(nested) = json.get(nested_key).and_then(|v| v.as_object()) {
            if let Some(arr) = nested.get("default_packages").and_then(|v| v.as_array()) {
                return arr
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect();
            }
        }
        vec![]
    }

    /// Migrate old flat keys to nested structure.
    ///
    /// Reads `default_uv_packages` and `default_conda_packages` from ROOT,
    /// splits comma values, stores them as nested lists, and deletes the old keys.
    fn migrate_flat_to_nested(&mut self) {
        // Migrate default_uv_packages -> uv.default_packages
        if let Some(val) = self.get_flat("default_uv_packages") {
            let packages = split_comma_list(&val);
            if !packages.is_empty() {
                self.put_list("uv.default_packages", &packages);
            }
            let _ = self.doc.delete(automerge::ROOT, "default_uv_packages");
            info!("[settings] Migrated default_uv_packages to uv.default_packages");
        }

        // Migrate default_conda_packages -> conda.default_packages
        if let Some(val) = self.get_flat("default_conda_packages") {
            let packages = split_comma_list(&val);
            if !packages.is_empty() {
                self.put_list("conda.default_packages", &packages);
            }
            let _ = self.doc.delete(automerge::ROOT, "default_conda_packages");
            info!("[settings] Migrated default_conda_packages to conda.default_packages");
        }
    }

    /// Migrate legacy null keep_alive_secs (from "forever" mode) to MAX_KEEP_ALIVE_SECS.
    ///
    /// Previously, `null` meant "keep alive forever". Now we use a fixed maximum of 7 days.
    /// This migration ensures users who had "forever" mode get the longest available duration
    /// instead of silently falling back to the 30-second default.
    fn migrate_null_keep_alive(&mut self) {
        if self.is_null("keep_alive_secs") {
            info!(
                "[settings] Migrating null keep_alive_secs to MAX_KEEP_ALIVE_SECS ({})",
                MAX_KEEP_ALIVE_SECS
            );
            self.put_u64("keep_alive_secs", MAX_KEEP_ALIVE_SECS);
        }
    }

    /// Load a settings document from raw bytes.
    pub fn load(data: &[u8]) -> Result<Self, AutomergeError> {
        let doc = AutoCommit::load(data)?;
        Ok(Self { doc })
    }

    /// Serialize the document to bytes for persistence.
    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    /// Save the document to a file.
    pub fn save_to_file(&mut self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = self.save();
        std::fs::write(path, data)
    }

    /// Write a human-readable JSON mirror of the settings (for fallback/inspection).
    ///
    /// Injects a `$schema` key pointing to the companion schema file so editors
    /// can provide autocomplete and validation.
    pub fn save_json_mirror(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let settings = self.get_all();
        let mut json_value = serde_json::to_value(&settings).map_err(std::io::Error::other)?;
        if let Some(obj) = json_value.as_object_mut() {
            obj.insert(
                "$schema".to_string(),
                serde_json::Value::String("./settings.schema.json".to_string()),
            );
        }
        let json = serde_json::to_string_pretty(&json_value).map_err(std::io::Error::other)?;
        std::fs::write(path, format!("{json}\n"))
    }

    // ── Scalar accessors ─────────────────────────────────────────────

    /// Read a scalar string from ROOT only (no dotted path support).
    fn get_flat(&self, key: &str) -> Option<String> {
        read_scalar_str(&self.doc, automerge::ROOT, key)
    }

    /// Get a scalar setting value, supporting dotted paths for nested maps.
    ///
    /// E.g. `"theme"` reads from ROOT, `"uv.some_key"` reads from the `uv` sub-map.
    pub fn get(&self, key: &str) -> Option<String> {
        if let Some((map_key, sub_key)) = key.split_once('.') {
            let map_id = self.get_map_id(map_key)?;
            read_scalar_str(&self.doc, map_id, sub_key)
        } else {
            self.get_flat(key)
        }
    }

    /// Get a boolean setting value from the root.
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.doc
            .get(automerge::ROOT, key)
            .ok()
            .flatten()
            .and_then(|(value, _)| match value {
                automerge::Value::Scalar(s) => match s.as_ref() {
                    automerge::ScalarValue::Boolean(b) => Some(*b),
                    // Also support string "true"/"false" for migration
                    automerge::ScalarValue::Str(s) => match s.as_str() {
                        "true" => Some(true),
                        "false" => Some(false),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            })
    }

    /// Set a boolean setting value at the root.
    pub fn put_bool(&mut self, key: &str, value: bool) {
        let _ = self.doc.put(automerge::ROOT, key, value);
    }

    /// Get a u64 setting value from the root.
    pub fn get_u64(&self, key: &str) -> Option<u64> {
        self.doc
            .get(automerge::ROOT, key)
            .ok()
            .flatten()
            .and_then(|(value, _)| match value {
                automerge::Value::Scalar(s) => match s.as_ref() {
                    // Use try_from to prevent negative values wrapping to huge u64
                    automerge::ScalarValue::Int(i) => u64::try_from(*i).ok(),
                    automerge::ScalarValue::Uint(u) => Some(*u),
                    // Also support string for migration/JSON compatibility
                    automerge::ScalarValue::Str(s) => s.parse().ok(),
                    _ => None,
                },
                _ => None,
            })
    }

    /// Set a u64 setting value at the root.
    ///
    /// Values are clamped to i64::MAX to prevent overflow during conversion.
    pub fn put_u64(&mut self, key: &str, value: u64) {
        // Clamp to i64::MAX to prevent overflow (Automerge stores as signed int)
        let clamped = value.min(i64::MAX as u64);
        let _ = self.doc.put(automerge::ROOT, key, clamped as i64);
    }

    /// Check if a key exists in the document but has a null value.
    fn is_null(&self, key: &str) -> bool {
        self.doc
            .get(automerge::ROOT, key)
            .ok()
            .flatten()
            .map(|(value, _)| matches!(value, automerge::Value::Scalar(s) if matches!(s.as_ref(), automerge::ScalarValue::Null)))
            .unwrap_or(false)
    }

    /// Set a scalar setting value, supporting dotted paths for nested maps.
    pub fn put(&mut self, key: &str, value: &str) {
        if let Some((map_key, sub_key)) = key.split_once('.') {
            let map_id = self.ensure_map(map_key);
            let _ = self.doc.put(&map_id, sub_key, value);
        } else {
            let _ = self.doc.put(automerge::ROOT, key, value);
        }
    }

    // ── List accessors ───────────────────────────────────────────────

    /// Read a list of strings at a dotted path (e.g. `"uv.default_packages"`).
    pub fn get_list(&self, key: &str) -> Vec<String> {
        let (map_key, sub_key) = match key.split_once('.') {
            Some(pair) => pair,
            None => return vec![],
        };
        let map_id = match self.get_map_id(map_key) {
            Some(id) => id,
            None => return vec![],
        };
        let list_id = match self.doc.get(&map_id, sub_key).ok().flatten() {
            Some((automerge::Value::Object(ObjType::List), id)) => id,
            _ => return vec![],
        };
        let len = self.doc.length(&list_id);
        (0..len)
            .filter_map(|i| {
                self.doc
                    .get(&list_id, i)
                    .ok()
                    .flatten()
                    .and_then(|(value, _)| match value {
                        automerge::Value::Scalar(s) => match s.as_ref() {
                            automerge::ScalarValue::Str(s) => Some(s.to_string()),
                            _ => None,
                        },
                        _ => None,
                    })
            })
            .collect()
    }

    /// Replace a list of strings at a dotted path.
    ///
    /// Deletes the existing list (if any) and creates a new one with the given items.
    pub fn put_list(&mut self, key: &str, values: &[String]) {
        let (map_key, sub_key) = match key.split_once('.') {
            Some(pair) => pair,
            None => return,
        };
        let map_id = self.ensure_map(map_key);

        // Delete existing value at this key (list or otherwise)
        let _ = self.doc.delete(&map_id, sub_key);

        // Create new list and insert items
        if let Ok(list_id) = self.doc.put_object(&map_id, sub_key, ObjType::List) {
            for (i, item) in values.iter().enumerate() {
                let _ = self.doc.insert(&list_id, i, item.as_str());
            }
        }
    }

    /// Set a value from a `serde_json::Value` — dispatches to `put` for strings,
    /// `put_list` for arrays, `put_bool` for booleans, or `put_u64` for numbers.
    /// Used by Tauri commands.
    pub fn put_value(&mut self, key: &str, value: &serde_json::Value) {
        match value {
            serde_json::Value::String(s) => self.put(key, s),
            serde_json::Value::Array(arr) => {
                let items: Vec<String> = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect();
                self.put_list(key, &items);
            }
            serde_json::Value::Bool(b) => self.put_bool(key, *b),
            serde_json::Value::Number(n) => {
                if let Some(u) = n.as_u64() {
                    self.put_u64(key, u);
                }
            }
            _ => {}
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────

    /// Look up a nested Map object at ROOT.
    fn get_map_id(&self, map_key: &str) -> Option<ObjId> {
        self.doc
            .get(automerge::ROOT, map_key)
            .ok()
            .flatten()
            .and_then(|(value, id)| match value {
                automerge::Value::Object(ObjType::Map) => Some(id),
                _ => None,
            })
    }

    /// Get or create a nested Map at ROOT.
    fn ensure_map(&mut self, map_key: &str) -> ObjId {
        if let Some(id) = self.get_map_id(map_key) {
            return id;
        }
        self.doc
            .put_object(automerge::ROOT, map_key, ObjType::Map)
            .expect("failed to create nested map")
    }

    // ── Aggregate accessor ───────────────────────────────────────────

    /// Get a snapshot of all settings.
    ///
    /// Reads from nested maps first, falling back to old flat keys for
    /// backward compatibility during upgrades.
    pub fn get_all(&self) -> SyncedSettings {
        let defaults = SyncedSettings::default();

        // Read uv packages: try nested list, fall back to flat comma string
        let uv_packages = {
            let nested = self.get_list("uv.default_packages");
            if !nested.is_empty() {
                nested
            } else if let Some(flat) = self.get_flat("default_uv_packages") {
                split_comma_list(&flat)
            } else {
                defaults.uv.default_packages.clone()
            }
        };

        // Read conda packages: try nested list, fall back to flat comma string
        let conda_packages = {
            let nested = self.get_list("conda.default_packages");
            if !nested.is_empty() {
                nested
            } else if let Some(flat) = self.get_flat("default_conda_packages") {
                split_comma_list(&flat)
            } else {
                defaults.conda.default_packages.clone()
            }
        };

        SyncedSettings {
            theme: self
                .get("theme")
                .and_then(|s| serde_json::from_str::<ThemeMode>(&format!("\"{s}\"")).ok())
                .unwrap_or(defaults.theme),
            default_runtime: self
                .get("default_runtime")
                .and_then(|s| s.parse().ok())
                .unwrap_or_default(),
            default_python_env: self
                .get("default_python_env")
                .and_then(|s| s.parse().ok())
                .unwrap_or_default(),
            uv: UvDefaults {
                default_packages: uv_packages,
            },
            conda: CondaDefaults {
                default_packages: conda_packages,
            },
            keep_alive_secs: self
                .get_u64("keep_alive_secs")
                .unwrap_or(defaults.keep_alive_secs),
        }
    }

    /// Generate a sync message to send to a peer.
    pub fn generate_sync_message(&mut self, peer_state: &mut sync::State) -> Option<sync::Message> {
        self.doc.sync().generate_sync_message(peer_state)
    }

    /// Receive and apply a sync message from a peer.
    pub fn receive_sync_message(
        &mut self,
        peer_state: &mut sync::State,
        message: sync::Message,
    ) -> Result<(), AutomergeError> {
        self.doc.sync().receive_sync_message(peer_state, message)
    }

    /// Selectively apply external JSON changes to the Automerge doc.
    ///
    /// Only updates fields that are **present** in the JSON and **differ** from
    /// the current document state. Returns `true` if any field was modified.
    pub(crate) fn apply_json_changes(&mut self, json: &serde_json::Value) -> bool {
        let mut changed = false;

        // Scalar fields — only update if present in JSON and different
        for key in &["theme", "default_runtime", "default_python_env"] {
            if let Some(value) = json.get(key).and_then(|v| v.as_str()) {
                let current = self.get(key);
                if current.as_deref() != Some(value) {
                    info!(
                        "[settings] apply_json_changes: {key} changed {:?} -> {value:?}",
                        current.as_deref()
                    );
                    self.put(key, value);
                    changed = true;
                }
            }
        }

        // UV packages
        if json.get("uv").is_some() {
            let uv_packages = Self::extract_packages_from_json(json, "uv");
            if self.get_list("uv.default_packages") != uv_packages {
                self.put_list("uv.default_packages", &uv_packages);
                changed = true;
            }
        }

        // Conda packages
        if json.get("conda").is_some() {
            let conda_packages = Self::extract_packages_from_json(json, "conda");
            if self.get_list("conda.default_packages") != conda_packages {
                self.put_list("conda.default_packages", &conda_packages);
                changed = true;
            }
        }

        // keep_alive_secs (numeric or null for forever)
        // keep_alive_secs: numeric value in seconds (invalid values are ignored)
        if let Some(new_secs) = json.get("keep_alive_secs").and_then(|v| v.as_u64()) {
            let current = self.get_u64("keep_alive_secs");
            if current != Some(new_secs) {
                info!(
                    "[settings] apply_json_changes: keep_alive_secs changed {:?} -> {}",
                    current, new_secs
                );
                self.put_u64("keep_alive_secs", new_secs);
                changed = true;
            }
        }

        changed
    }
}

impl Default for SettingsDoc {
    fn default() -> Self {
        Self::new()
    }
}

// ── Free helpers ─────────────────────────────────────────────────────

/// Read a scalar string value from any Automerge object.
fn read_scalar_str<O: AsRef<ObjId>>(doc: &AutoCommit, obj: O, key: &str) -> Option<String> {
    doc.get(obj, key)
        .ok()
        .flatten()
        .and_then(|(value, _)| match value {
            automerge::Value::Scalar(s) => match s.as_ref() {
                automerge::ScalarValue::Str(s) => Some(s.to_string()),
                _ => None,
            },
            _ => None,
        })
}

/// Split a comma-separated string into a list of trimmed, non-empty strings.
pub fn split_comma_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Read a list of strings from a nested Automerge map within a raw `AutoCommit`.
///
/// Used by `sync_client::get_all_from_doc` which operates on bare docs.
pub fn read_nested_list(doc: &AutoCommit, map_key: &str, sub_key: &str) -> Vec<String> {
    let map_id = match doc.get(automerge::ROOT, map_key).ok().flatten() {
        Some((automerge::Value::Object(ObjType::Map), id)) => id,
        _ => return vec![],
    };
    let list_id = match doc.get(&map_id, sub_key).ok().flatten() {
        Some((automerge::Value::Object(ObjType::List), id)) => id,
        _ => return vec![],
    };
    let len = doc.length(&list_id);
    (0..len)
        .filter_map(|i| {
            doc.get(&list_id, i)
                .ok()
                .flatten()
                .and_then(|(value, _)| match value {
                    automerge::Value::Scalar(s) => match s.as_ref() {
                        automerge::ScalarValue::Str(s) => Some(s.to_string()),
                        _ => None,
                    },
                    _ => None,
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_new_has_defaults() {
        let doc = SettingsDoc::new();
        let settings = doc.get_all();
        assert_eq!(settings.theme, ThemeMode::System);
        assert_eq!(settings.default_runtime, Runtime::Python);
        assert_eq!(settings.default_python_env, PythonEnvType::Uv);
        assert!(settings.uv.default_packages.is_empty());
        assert!(settings.conda.default_packages.is_empty());
    }

    #[test]
    fn test_put_and_get_scalar() {
        let mut doc = SettingsDoc::new();
        doc.put("theme", "dark");
        assert_eq!(doc.get("theme"), Some("dark".to_string()));
    }

    #[test]
    fn test_get_nonexistent_key() {
        let doc = SettingsDoc::new();
        assert_eq!(doc.get("nonexistent"), None);
    }

    #[test]
    fn test_put_and_get_list() {
        let mut doc = SettingsDoc::new();
        doc.put_list(
            "uv.default_packages",
            &["numpy".to_string(), "pandas".to_string()],
        );

        let packages = doc.get_list("uv.default_packages");
        assert_eq!(packages, vec!["numpy", "pandas"]);
    }

    #[test]
    fn test_put_list_replaces_existing() {
        let mut doc = SettingsDoc::new();
        doc.put_list("uv.default_packages", &["numpy".to_string()]);
        doc.put_list(
            "uv.default_packages",
            &["pandas".to_string(), "scipy".to_string()],
        );

        let packages = doc.get_list("uv.default_packages");
        assert_eq!(packages, vec!["pandas", "scipy"]);
    }

    #[test]
    fn test_get_list_empty_by_default() {
        let doc = SettingsDoc::new();
        let packages = doc.get_list("uv.default_packages");
        assert!(packages.is_empty());
    }

    #[test]
    fn test_put_value_string() {
        let mut doc = SettingsDoc::new();
        doc.put_value("theme", &serde_json::json!("dark"));
        assert_eq!(doc.get("theme"), Some("dark".to_string()));
    }

    #[test]
    fn test_put_value_array() {
        let mut doc = SettingsDoc::new();
        doc.put_value(
            "uv.default_packages",
            &serde_json::json!(["numpy", "pandas"]),
        );
        assert_eq!(doc.get_list("uv.default_packages"), vec!["numpy", "pandas"]);
    }

    #[test]
    fn test_get_all_with_packages() {
        let mut doc = SettingsDoc::new();
        doc.put_list(
            "uv.default_packages",
            &["numpy".to_string(), "pandas".to_string()],
        );
        doc.put_list("conda.default_packages", &["scipy".to_string()]);

        let settings = doc.get_all();
        assert_eq!(settings.uv.default_packages, vec!["numpy", "pandas"]);
        assert_eq!(settings.conda.default_packages, vec!["scipy"]);
    }

    #[test]
    fn test_save_and_load() {
        let mut doc = SettingsDoc::new();
        doc.put("theme", "light");
        doc.put_list("uv.default_packages", &["numpy".to_string()]);

        let bytes = doc.save();
        let loaded = SettingsDoc::load(&bytes).unwrap();

        assert_eq!(loaded.get("theme"), Some("light".to_string()));
        assert_eq!(loaded.get("default_runtime"), Some("python".to_string()));
        assert_eq!(loaded.get_list("uv.default_packages"), vec!["numpy"]);
    }

    #[test]
    fn test_save_to_file_and_load_or_create() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.automerge");

        let mut doc = SettingsDoc::new();
        doc.put("theme", "dark");
        doc.put_list(
            "conda.default_packages",
            &["scipy".to_string(), "numpy".to_string()],
        );
        doc.save_to_file(&path).unwrap();

        let loaded = SettingsDoc::load_or_create(&path, None);
        assert_eq!(loaded.get("theme"), Some("dark".to_string()));
        assert_eq!(
            loaded.get_list("conda.default_packages"),
            vec!["scipy", "numpy"]
        );
    }

    #[test]
    fn test_migrate_flat_to_nested() {
        // Simulate an old Automerge doc with flat comma-separated keys
        let mut doc = AutoCommit::new();
        let _ = doc.put(automerge::ROOT, "theme", "dark");
        let _ = doc.put(automerge::ROOT, "default_runtime", "python");
        let _ = doc.put(automerge::ROOT, "default_python_env", "uv");
        let _ = doc.put(
            automerge::ROOT,
            "default_uv_packages",
            "numpy, pandas, matplotlib",
        );
        let _ = doc.put(automerge::ROOT, "default_conda_packages", "scipy");

        let bytes = doc.save();

        // Load via load_or_create which triggers migration
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.automerge");
        std::fs::write(&path, bytes).unwrap();

        let loaded = SettingsDoc::load_or_create(&path, None);
        let settings = loaded.get_all();

        assert_eq!(settings.theme, ThemeMode::Dark);
        assert_eq!(
            settings.uv.default_packages,
            vec!["numpy", "pandas", "matplotlib"]
        );
        assert_eq!(settings.conda.default_packages, vec!["scipy"]);

        // Old flat keys should be gone
        assert_eq!(loaded.get_flat("default_uv_packages"), None);
        assert_eq!(loaded.get_flat("default_conda_packages"), None);
    }

    #[test]
    fn test_migrate_from_json_nested_format() {
        let tmp = TempDir::new().unwrap();
        let automerge_path = tmp.path().join("settings.automerge");
        let json_path = tmp.path().join("settings.json");

        // Write new-format settings.json
        std::fs::write(
            &json_path,
            r#"{"default_runtime":"python","uv":{"default_packages":["numpy","pandas"]},"conda":{"default_packages":["scipy"]}}"#,
        )
        .unwrap();

        let doc = SettingsDoc::load_or_create(&automerge_path, Some(&json_path));
        let settings = doc.get_all();

        assert_eq!(settings.uv.default_packages, vec!["numpy", "pandas"]);
        assert_eq!(settings.conda.default_packages, vec!["scipy"]);
    }

    #[test]
    fn test_load_or_create_defaults() {
        let tmp = TempDir::new().unwrap();
        let automerge_path = tmp.path().join("settings.automerge");

        let doc = SettingsDoc::load_or_create(&automerge_path, None);
        assert_eq!(doc.get_all(), SyncedSettings::default());
    }

    #[test]
    fn test_json_mirror() {
        let tmp = TempDir::new().unwrap();
        let json_path = tmp.path().join("settings.json");

        let mut doc = SettingsDoc::new();
        doc.put("theme", "dark");
        doc.put_list(
            "uv.default_packages",
            &["numpy".to_string(), "pandas".to_string()],
        );
        doc.save_json_mirror(&json_path).unwrap();

        let contents = std::fs::read_to_string(&json_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed["$schema"], "./settings.schema.json");
        assert_eq!(parsed["theme"], "dark");
        assert_eq!(parsed["uv"]["default_packages"][0], "numpy");
        assert_eq!(parsed["uv"]["default_packages"][1], "pandas");
    }

    #[test]
    fn test_apply_json_changes_ignores_schema_key() {
        let mut doc = SettingsDoc::new();
        let json = serde_json::json!({
            "$schema": "./settings.schema.json",
            "theme": "dark",
        });
        let changed = doc.apply_json_changes(&json);
        assert!(changed);
        assert_eq!(doc.get("theme"), Some("dark".to_string()));
    }

    #[test]
    fn test_generate_settings_schema() {
        let schema = generate_settings_schema().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&schema).unwrap();
        // Should be a valid JSON Schema with properties
        let schema_str = &schema;
        assert!(schema_str.contains("theme"));
        assert!(schema_str.contains("default_runtime"));
        assert!(schema_str.contains("default_python_env"));
        // Should have known values as examples for editor autocomplete
        assert!(schema_str.contains("python"));
        assert!(schema_str.contains("deno"));
        assert!(schema_str.contains("uv"));
        assert!(schema_str.contains("conda"));
        // Should be a proper JSON Schema object
        assert!(parsed.is_object());
    }

    #[test]
    fn test_schema_key_ignored_during_deserialization() {
        let json = r#"{
            "$schema": "./settings.schema.json",
            "theme": "dark",
            "default_runtime": "deno",
            "default_python_env": "conda",
            "uv": { "default_packages": [] },
            "conda": { "default_packages": [] }
        }"#;
        let parsed: SyncedSettings = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.theme, ThemeMode::Dark);
        assert_eq!(parsed.default_runtime, Runtime::Deno);
        assert_eq!(parsed.default_python_env, PythonEnvType::Conda);
    }

    #[test]
    fn test_sync_between_two_docs() {
        let mut server = SettingsDoc::new();
        server.put("theme", "dark");
        server.put_list("uv.default_packages", &["numpy".to_string()]);

        // Client starts empty — avoids conflicting object creation for nested
        // maps (both docs creating their own "uv" Map independently would cause
        // Automerge CRDT conflicts that resolve nondeterministically).
        let mut client = SettingsDoc {
            doc: AutoCommit::new(),
        };

        let mut server_state = sync::State::new();
        let mut client_state = sync::State::new();

        for _ in 0..10 {
            if let Some(msg) = client.generate_sync_message(&mut client_state) {
                server.receive_sync_message(&mut server_state, msg).unwrap();
            }
            if let Some(msg) = server.generate_sync_message(&mut server_state) {
                client.receive_sync_message(&mut client_state, msg).unwrap();
            }
        }

        assert_eq!(client.get("theme"), Some("dark".to_string()));
        assert_eq!(client.get("default_runtime"), Some("python".to_string()));
        assert_eq!(client.get_list("uv.default_packages"), vec!["numpy"]);
    }

    #[test]
    fn test_concurrent_writes_merge() {
        let mut server = SettingsDoc::new();
        let mut client = SettingsDoc::new();

        let mut server_state = sync::State::new();
        let mut client_state = sync::State::new();

        // Sync initial state
        for _ in 0..10 {
            if let Some(msg) = client.generate_sync_message(&mut client_state) {
                server.receive_sync_message(&mut server_state, msg).unwrap();
            }
            if let Some(msg) = server.generate_sync_message(&mut server_state) {
                client.receive_sync_message(&mut client_state, msg).unwrap();
            }
        }

        // Both make different changes
        server.put("theme", "dark");
        client.put("default_runtime", "deno");

        // Sync again
        for _ in 0..10 {
            if let Some(msg) = client.generate_sync_message(&mut client_state) {
                server.receive_sync_message(&mut server_state, msg).unwrap();
            }
            if let Some(msg) = server.generate_sync_message(&mut server_state) {
                client.receive_sync_message(&mut client_state, msg).unwrap();
            }
        }

        assert_eq!(server.get("theme"), Some("dark".to_string()));
        assert_eq!(server.get("default_runtime"), Some("deno".to_string()));
        assert_eq!(client.get("theme"), Some("dark".to_string()));
        assert_eq!(client.get("default_runtime"), Some("deno".to_string()));
    }

    #[test]
    fn test_split_comma_list() {
        assert_eq!(
            split_comma_list("numpy, pandas, matplotlib"),
            vec!["numpy", "pandas", "matplotlib"]
        );
        assert_eq!(split_comma_list(""), Vec::<String>::new());
        assert_eq!(split_comma_list("  "), Vec::<String>::new());
        assert_eq!(split_comma_list("numpy"), vec!["numpy"]);
    }

    #[test]
    fn test_nested_scalar_in_map() {
        let mut doc = SettingsDoc::new();
        // Write a scalar into a nested map (for future settings like conda channels)
        doc.put("uv.some_future_setting", "value");
        assert_eq!(doc.get("uv.some_future_setting"), Some("value".to_string()));
    }

    #[test]
    fn test_ensure_map_creates_if_missing() {
        let mut doc = SettingsDoc::new();
        // Put into a map that doesn't exist yet
        doc.put("new_section.key", "value");
        assert_eq!(doc.get("new_section.key"), Some("value".to_string()));
    }

    #[test]
    fn test_apply_json_changes_detects_difference() {
        let mut doc = SettingsDoc::new();
        assert_eq!(doc.get("theme"), Some("system".to_string()));

        let json = serde_json::json!({
            "theme": "dark",
            "default_runtime": "deno",
        });
        let changed = doc.apply_json_changes(&json);
        assert!(changed);
        assert_eq!(doc.get("theme"), Some("dark".to_string()));
        assert_eq!(doc.get("default_runtime"), Some("deno".to_string()));
        // Unchanged fields stay the same
        assert_eq!(doc.get("default_python_env"), Some("uv".to_string()));
    }

    #[test]
    fn test_apply_json_changes_no_change_when_matching() {
        let doc = SettingsDoc::new();
        let settings = doc.get_all();

        // Write current values back — should detect no change
        let json = serde_json::to_value(&settings).unwrap();
        let mut doc = SettingsDoc::new();
        let changed = doc.apply_json_changes(&json);
        assert!(!changed);
    }

    #[test]
    fn test_apply_json_changes_skips_absent_fields() {
        let mut doc = SettingsDoc::new();
        doc.put("theme", "dark");

        // JSON without theme key — should NOT reset theme
        let json = serde_json::json!({
            "default_runtime": "python",
        });
        let changed = doc.apply_json_changes(&json);
        assert!(!changed); // runtime already "python"
        assert_eq!(doc.get("theme"), Some("dark".to_string())); // preserved
    }

    #[test]
    fn test_apply_json_changes_nested_packages() {
        let mut doc = SettingsDoc::new();

        let json = serde_json::json!({
            "uv": { "default_packages": ["numpy", "pandas"] },
            "conda": { "default_packages": ["scipy"] },
        });
        let changed = doc.apply_json_changes(&json);
        assert!(changed);
        assert_eq!(doc.get_list("uv.default_packages"), vec!["numpy", "pandas"]);
        assert_eq!(doc.get_list("conda.default_packages"), vec!["scipy"]);
    }

    #[test]
    fn test_apply_json_changes_packages_no_change() {
        let mut doc = SettingsDoc::new();
        doc.put_list(
            "uv.default_packages",
            &["numpy".to_string(), "pandas".to_string()],
        );

        // Same packages — should detect no change
        let json = serde_json::json!({
            "uv": { "default_packages": ["numpy", "pandas"] },
        });
        let changed = doc.apply_json_changes(&json);
        assert!(!changed);
    }

    #[test]
    fn test_keep_alive_secs_valid_number() {
        let mut doc = SettingsDoc::new();
        doc.put_u64("keep_alive_secs", 60);

        let result = doc.get_u64("keep_alive_secs");
        assert_eq!(result, Some(60));

        let settings = doc.get_all();
        assert_eq!(settings.keep_alive_secs, 60);
    }

    #[test]
    fn test_keep_alive_secs_default() {
        let doc = SettingsDoc::new();
        let settings = doc.get_all();

        // Default should be 30 seconds
        assert_eq!(settings.keep_alive_secs, DEFAULT_KEEP_ALIVE_SECS);
    }

    #[test]
    fn test_apply_json_changes_keep_alive_valid_number() {
        let mut doc = SettingsDoc::new();

        let json = serde_json::json!({
            "keep_alive_secs": 120
        });
        let changed = doc.apply_json_changes(&json);
        assert!(changed);

        let settings = doc.get_all();
        assert_eq!(settings.keep_alive_secs, 120);
    }

    #[test]
    fn test_apply_json_changes_keep_alive_invalid_ignored() {
        let mut doc = SettingsDoc::new();
        // Set a known value first
        doc.put_u64("keep_alive_secs", 60);

        // Invalid values should be ignored
        // Test negative number (can't be represented as u64 in JSON)
        let json = serde_json::json!({
            "keep_alive_secs": -1
        });
        let changed = doc.apply_json_changes(&json);
        assert!(!changed); // Should be no change

        let settings = doc.get_all();
        assert_eq!(settings.keep_alive_secs, 60); // Original preserved

        // Test string value
        let json = serde_json::json!({
            "keep_alive_secs": "30"
        });
        let changed = doc.apply_json_changes(&json);
        assert!(!changed);

        let settings = doc.get_all();
        assert_eq!(settings.keep_alive_secs, 60); // Original preserved

        // Test null value (should be ignored, not treated specially)
        let json = serde_json::json!({
            "keep_alive_secs": null
        });
        let changed = doc.apply_json_changes(&json);
        assert!(!changed);

        let settings = doc.get_all();
        assert_eq!(settings.keep_alive_secs, 60); // Original preserved
    }

    #[test]
    fn test_get_u64_negative_int_returns_none() {
        use automerge::AutoCommit;

        // Manually create a doc with a negative Int value
        let mut automerge_doc = AutoCommit::new();
        let _ = automerge_doc.put(automerge::ROOT, "keep_alive_secs", -5_i64);

        // Wrap in SettingsDoc
        let doc = SettingsDoc { doc: automerge_doc };

        // Negative Int should return None (invalid)
        let result = doc.get_u64("keep_alive_secs");
        assert_eq!(result, None);
    }

    #[test]
    fn test_migrate_null_keep_alive_to_max() {
        use automerge::AutoCommit;

        // Manually create a doc with null keep_alive_secs (legacy "forever" mode)
        let mut automerge_doc = AutoCommit::new();
        let _ = automerge_doc.put(
            automerge::ROOT,
            "keep_alive_secs",
            automerge::ScalarValue::Null,
        );

        let mut doc = SettingsDoc { doc: automerge_doc };

        // Verify it's detected as null
        assert!(doc.is_null("keep_alive_secs"));
        assert_eq!(doc.get_u64("keep_alive_secs"), None);

        // Run migration
        doc.migrate_null_keep_alive();

        // Should now be MAX_KEEP_ALIVE_SECS
        assert!(!doc.is_null("keep_alive_secs"));
        assert_eq!(doc.get_u64("keep_alive_secs"), Some(MAX_KEEP_ALIVE_SECS));

        let settings = doc.get_all();
        assert_eq!(settings.keep_alive_secs, MAX_KEEP_ALIVE_SECS);
    }

    #[test]
    fn test_from_json_null_keep_alive_migrates_to_max() {
        // JSON with null keep_alive_secs (legacy "forever" mode)
        let json = serde_json::json!({
            "theme": "dark",
            "keep_alive_secs": null
        });

        let doc = SettingsDoc::from_json(&json);
        let settings = doc.get_all();

        // Should be MAX_KEEP_ALIVE_SECS, not the default 30s
        assert_eq!(settings.keep_alive_secs, MAX_KEEP_ALIVE_SECS);
        assert_eq!(settings.theme, ThemeMode::Dark);
    }

    #[test]
    fn test_put_u64_clamps_extreme_values() {
        let mut doc = SettingsDoc::new();

        // Very large value that would overflow i64
        let extreme_value = u64::MAX;
        doc.put_u64("keep_alive_secs", extreme_value);

        // Should be clamped to i64::MAX
        let result = doc.get_u64("keep_alive_secs");
        assert_eq!(result, Some(i64::MAX as u64));
    }

    #[test]
    fn test_is_null_returns_false_for_numeric() {
        let doc = SettingsDoc::new();
        // New doc has numeric keep_alive_secs (default 30)
        assert!(!doc.is_null("keep_alive_secs"));
    }

    #[test]
    fn test_is_null_returns_false_for_missing_key() {
        use automerge::AutoCommit;

        // Empty doc with no keep_alive_secs key
        let automerge_doc = AutoCommit::new();
        let doc = SettingsDoc { doc: automerge_doc };

        // Missing key is not the same as null
        assert!(!doc.is_null("keep_alive_secs"));
    }
}
