use serde_json::{json, Value};
use std::env;
use std::path::{Path, PathBuf};

fn merge_json(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Object(base_map), Value::Object(overlay_map)) => {
            for (key, value) in overlay_map {
                merge_json(base_map.entry(key).or_insert(Value::Null), value);
            }
        }
        (slot, value) => *slot = value,
    }
}

fn target_sidecar_path(manifest_dir: &Path, target: &str, binary_name: &str) -> PathBuf {
    let executable_suffix = if target.contains("windows") {
        ".exe"
    } else {
        ""
    };
    manifest_dir
        .join("binaries")
        .join(format!("{binary_name}-{target}{executable_suffix}"))
}

fn maybe_disable_external_bin_for_local_checks() {
    let profile = env::var("PROFILE").unwrap_or_default();
    if profile == "release" {
        return;
    }

    let Ok(manifest_dir_str) = env::var("CARGO_MANIFEST_DIR") else {
        return;
    };
    let manifest_dir = PathBuf::from(manifest_dir_str);
    let Ok(target) = env::var("TARGET") else {
        return;
    };

    let expected_sidecars = [
        target_sidecar_path(&manifest_dir, &target, "runtimed"),
        target_sidecar_path(&manifest_dir, &target, "runt"),
        target_sidecar_path(&manifest_dir, &target, "nteract-mcp"),
    ];

    for path in &expected_sidecars {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let missing_sidecars: Vec<_> = expected_sidecars
        .iter()
        .filter(|path| !path.exists())
        .collect();

    if missing_sidecars.is_empty() {
        return;
    }

    let mut config = env::var("TAURI_CONFIG")
        .ok()
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_else(|| json!({}));
    merge_json(&mut config, json!({ "bundle": { "externalBin": [] } }));
    if let Ok(serialized) = serde_json::to_string(&config) {
        env::set_var("TAURI_CONFIG", serialized);
    }

    let missing_paths = missing_sidecars
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "cargo:warning=Sidecar binaries missing ({missing_paths}); skipping bundle.externalBin for non-release builds so cargo check can run without `cargo xtask build`."
    );
}

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    build_metadata::emit_git_rerun_hints();
    build_metadata::write_git_metadata(&out_dir);

    // Re-run if frontend dist changes (ensures fresh frontend is embedded).
    // This is the only non-git watcher — Phase 2 builds the frontend, then
    // Phase 3 (cargo tauri build) picks up the updated dist/.
    println!("cargo:rerun-if-changed=../../apps/notebook/dist");

    maybe_disable_external_bin_for_local_checks();

    tauri_build::build()
}
