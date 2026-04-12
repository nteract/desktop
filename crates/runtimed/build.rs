use std::process::Command;

fn main() {
    // Rebuild when plugin assets change (committed via Git LFS)
    for file in [
        "markdown.js",
        "markdown.css",
        "plotly.js",
        "vega.js",
        "leaflet.js",
        "leaflet.css",
    ] {
        println!("cargo:rerun-if-changed=../runt-mcp/assets/plugins/{file}");
    }

    // Also rerun when this build script changes (since we emit
    // rerun-if-changed above, cargo won't use its default behavior).
    println!("cargo:rerun-if-changed=build.rs");

    // Capture short commit hash for version-mismatch detection.
    let commit = git_commit_short();
    println!("cargo:rustc-env=GIT_COMMIT={commit}");

    // We intentionally do NOT watch .git/HEAD or refs — that causes
    // recompilation of this crate and all dependents on every commit,
    // branch switch, pull, or fetch. The hash is refreshed whenever
    // this build script reruns (plugin asset change or build.rs edit).
    // CI always starts clean so release builds always get the right hash.
}

fn git_commit_short() -> String {
    Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
