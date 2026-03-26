use serde_json::{json, Value};
use std::env;
use std::path::PathBuf;
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

fn target_sidecar_path(manifest_dir: &PathBuf, target: &str, binary_name: &str) -> PathBuf {
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

    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set"));
    let target = env::var("TARGET").expect("TARGET must be set");

    let expected_sidecars = [
        target_sidecar_path(&manifest_dir, &target, "runtimed"),
        target_sidecar_path(&manifest_dir, &target, "runt"),
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
    env::set_var(
        "TAURI_CONFIG",
        serde_json::to_string(&config).expect("TAURI_CONFIG override must serialize"),
    );

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
    // Capture git branch name
    let branch = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Capture short commit hash
    let commit = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Capture commit date (used as release date in About metadata)
    let release_date = Command::new("git")
        .args(["show", "-s", "--format=%cs", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Set environment variables for use in Rust code
    println!("cargo:rustc-env=GIT_BRANCH={}", branch);
    println!("cargo:rustc-env=GIT_COMMIT={}", commit);
    println!("cargo:rustc-env=GIT_COMMIT_DATE={}", release_date);

    // Re-run if git HEAD changes (detects branch switches, commits)
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");

    // Re-run if frontend dist changes (ensures fresh frontend is embedded)
    println!("cargo:rerun-if-changed=../../apps/notebook/dist");

    maybe_disable_external_bin_for_local_checks();

    tauri_build::build()
}
