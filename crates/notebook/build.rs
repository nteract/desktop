use serde_json::{json, Value};
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

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
        target_sidecar_path(&manifest_dir, &target, "mcpb-runt"),
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
    // Capture git metadata for About menu and version strings.
    let branch = git_output(&["rev-parse", "--abbrev-ref", "HEAD"]);
    let commit = git_output(&["rev-parse", "--short=7", "HEAD"]);
    let release_date = git_output(&["show", "-s", "--format=%cs", "HEAD"]);

    println!("cargo:rustc-env=GIT_BRANCH={branch}");
    println!("cargo:rustc-env=GIT_COMMIT={commit}");
    println!("cargo:rustc-env=GIT_COMMIT_DATE={release_date}");

    // Re-run if frontend dist changes (ensures fresh frontend is embedded).
    // This is the only non-git watcher — Phase 2 builds the frontend, then
    // Phase 3 (cargo tauri build) picks up the updated dist/.
    println!("cargo:rerun-if-changed=../../apps/notebook/dist");

    // We intentionally do NOT watch .git/HEAD, .git/index, or refs.
    // That caused recompilation of notebook + all dependents on every
    // commit, branch switch, pull, or fetch. The git metadata is refreshed
    // whenever dist/ changes (every build) or this build script changes.
    // CI always starts clean so release builds always get the right hash.

    maybe_disable_external_bin_for_local_checks();

    tauri_build::build()
}

fn git_output(args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
