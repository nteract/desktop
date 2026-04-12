use std::process::Command;

fn main() {
    // Capture short commit hash for version-mismatch detection.
    // This ensures the daemon gets restarted when the binary changes,
    // even if the crate version (Cargo.toml) hasn't been bumped.
    let commit = git_commit_short();
    println!("cargo:rustc-env=GIT_COMMIT={commit}");

    // No rerun-if-changed directives — cargo's default behavior reruns
    // this script when any file in the package changes, which is exactly
    // when we want a fresh commit hash. We intentionally do NOT watch
    // .git/HEAD or refs — that causes recompilation of this crate (and
    // all dependents) on every commit, branch switch, pull, or fetch.
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
