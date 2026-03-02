//! Application settings persistence for notebook preferences.
//!
//! Settings are stored in a JSON file in the user's config directory:
//! - macOS: ~/Library/Application Support/nteract/settings.json
//! - Linux: ~/.config/nteract/settings.json
//! - Windows: C:\Users\<User>\AppData\Roaming\nteract\settings.json
//!
//! The daemon (runtimed) is the sole writer to settings.json to prevent race
//! conditions when multiple notebook windows are open. This module only reads
//! settings for fallback when the daemon is unavailable.
//!
//! Uses `runtimed::settings_doc::SyncedSettings` as the canonical settings type.

use runtimed::settings_doc::SyncedSettings;
use std::path::PathBuf;

// Re-export types that notebook code uses from runtimed
pub use runtimed::runtime::Runtime;
pub use runtimed::settings_doc::{CondaDefaults, PythonEnvType, ThemeMode, UvDefaults};

/// Get the path to the settings file
fn settings_path() -> PathBuf {
    runtimed::settings_json_path()
}

/// Load settings from disk, returning defaults if file doesn't exist.
///
/// Uses per-field fallback so a single invalid value (e.g. a bad enum string
/// from a manual edit) doesn't wipe all other settings back to defaults.
pub fn load_settings() -> SyncedSettings {
    let path = settings_path();
    if !path.exists() {
        return SyncedSettings::default();
    }
    let contents = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return SyncedSettings::default(),
    };

    // Fast path: if the whole file deserializes cleanly, use it directly.
    if let Ok(settings) = serde_json::from_str::<SyncedSettings>(&contents) {
        return settings;
    }

    // Slow path: parse as Value, extract each field individually so one bad
    // value doesn't lose every other valid setting.
    let json: serde_json::Value = match serde_json::from_str(&contents) {
        Ok(v) => v,
        Err(_) => return SyncedSettings::default(),
    };
    let defaults = SyncedSettings::default();
    SyncedSettings {
        theme: json
            .get("theme")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(defaults.theme),
        default_runtime: json
            .get("default_runtime")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(defaults.default_runtime),
        default_python_env: json
            .get("default_python_env")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(defaults.default_python_env),
        uv: json
            .get("uv")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(defaults.uv),
        conda: json
            .get("conda")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(defaults.conda),
        keep_alive_secs: json
            .get("keep_alive_secs")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(defaults.keep_alive_secs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_settings() {
        let settings = SyncedSettings::default();
        assert_eq!(settings.theme, ThemeMode::System);
        assert_eq!(settings.default_runtime, Runtime::Python);
        assert_eq!(settings.default_python_env, PythonEnvType::Uv);
        assert!(settings.uv.default_packages.is_empty());
        assert!(settings.conda.default_packages.is_empty());
    }

    #[test]
    fn test_settings_serde_nested_format() {
        let settings = SyncedSettings {
            theme: ThemeMode::Dark,
            default_runtime: Runtime::Deno,
            default_python_env: PythonEnvType::Uv,
            uv: UvDefaults {
                default_packages: vec!["numpy".into(), "pandas".into()],
            },
            conda: CondaDefaults::default(),
            keep_alive_secs: 30,
        };

        let json = serde_json::to_string(&settings).unwrap();
        let parsed: SyncedSettings = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.theme, ThemeMode::Dark);
        assert_eq!(parsed.default_runtime, Runtime::Deno);
        assert_eq!(parsed.default_python_env, PythonEnvType::Uv);
        assert_eq!(parsed.uv.default_packages, vec!["numpy", "pandas"]);
    }

    #[test]
    fn test_deserialize_nested_format() {
        let json = r#"{
            "theme": "dark",
            "default_runtime": "python",
            "default_python_env": "uv",
            "uv": { "default_packages": ["numpy", "pandas"] },
            "conda": { "default_packages": ["scipy"] }
        }"#;
        let parsed: SyncedSettings = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.theme, ThemeMode::Dark);
        assert_eq!(parsed.uv.default_packages, vec!["numpy", "pandas"]);
        assert_eq!(parsed.conda.default_packages, vec!["scipy"]);
    }

    #[test]
    fn test_deserialize_missing_fields_defaults() {
        let json = r#"{"default_runtime": "python"}"#;
        let parsed: SyncedSettings = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.theme, ThemeMode::System);
        assert!(parsed.uv.default_packages.is_empty());
        assert!(parsed.conda.default_packages.is_empty());
    }

    #[test]
    fn test_python_env_type_serde() {
        let uv = PythonEnvType::Uv;
        let conda = PythonEnvType::Conda;

        assert_eq!(serde_json::to_string(&uv).unwrap(), "\"uv\"");
        assert_eq!(serde_json::to_string(&conda).unwrap(), "\"conda\"");

        let parsed_uv: PythonEnvType = serde_json::from_str("\"uv\"").unwrap();
        let parsed_conda: PythonEnvType = serde_json::from_str("\"conda\"").unwrap();

        assert_eq!(parsed_uv, PythonEnvType::Uv);
        assert_eq!(parsed_conda, PythonEnvType::Conda);
    }

    #[test]
    fn test_settings_path_is_valid() {
        let path = settings_path();
        assert!(path.ends_with("nteract/settings.json"));
    }

    #[test]
    fn test_schema_key_in_json_ignored_during_deserialization() {
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
    fn test_unknown_enum_round_trips_through_settings() {
        // Unknown runtime/env values should survive a load -> save round-trip.
        let json = r#"{
            "theme": "dark",
            "default_runtime": "julia",
            "default_python_env": "mamba",
            "uv": { "default_packages": ["numpy"] },
            "conda": { "default_packages": [] }
        }"#;
        let parsed: SyncedSettings = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.theme, ThemeMode::Dark);
        assert_eq!(parsed.default_runtime, Runtime::Other("julia".into()));
        assert_eq!(
            parsed.default_python_env,
            PythonEnvType::Other("mamba".into())
        );
        assert_eq!(parsed.uv.default_packages, vec!["numpy"]);

        // Re-serialize and verify the unknown values survive
        let reserialized = serde_json::to_string(&parsed).unwrap();
        assert!(reserialized.contains("\"julia\""));
        assert!(reserialized.contains("\"mamba\""));
    }

    #[test]
    fn test_load_settings_wrong_type_preserves_valid_fields() {
        // A non-string value for an enum field (e.g. a number) should fail
        // per-field deserialization but not lose other valid fields.
        let json = r#"{
            "theme": "dark",
            "default_runtime": 42,
            "default_python_env": "uv",
            "uv": { "default_packages": ["numpy"] },
            "conda": { "default_packages": [] }
        }"#;
        // Strict deser should fail (42 is not a valid string for Runtime)
        assert!(serde_json::from_str::<SyncedSettings>(json).is_err());
        // Per-field fallback: parse as Value, extract individually
        let json_val: serde_json::Value = serde_json::from_str(json).unwrap();
        let defaults = SyncedSettings::default();
        let settings = SyncedSettings {
            theme: json_val
                .get("theme")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or(defaults.theme),
            default_runtime: json_val
                .get("default_runtime")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or(defaults.default_runtime),
            default_python_env: json_val
                .get("default_python_env")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or(defaults.default_python_env),
            uv: json_val
                .get("uv")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or(defaults.uv),
            conda: json_val
                .get("conda")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or(defaults.conda),
            keep_alive_secs: json_val
                .get("keep_alive_secs")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or(defaults.keep_alive_secs),
        };
        // Valid fields are preserved
        assert_eq!(settings.theme, ThemeMode::Dark);
        assert_eq!(settings.uv.default_packages, vec!["numpy"]);
        assert_eq!(settings.default_python_env, PythonEnvType::Uv);
        // Non-string field falls back to default
        assert_eq!(settings.default_runtime, Runtime::Python);
    }
}
