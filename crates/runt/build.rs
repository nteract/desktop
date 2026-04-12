use std::process::Command;

fn main() {
    let commit = git_commit_short();
    println!("cargo:rustc-env=GIT_COMMIT={commit}");

    let variant = std::env::var("RUNT_VARIANT").unwrap_or_default();
    println!("cargo:rustc-env=RUNT_VARIANT={variant}");
    println!("cargo:rerun-if-env-changed=RUNT_VARIANT");

    // Also rerun when this build script changes (since we emit
    // rerun-if-changed above, cargo won't use its default behavior).
    println!("cargo:rerun-if-changed=build.rs");

    // We intentionally do NOT watch .git/HEAD or refs — that causes
    // recompilation on every commit, branch switch, pull, or fetch.
    // The hash is refreshed when build.rs or RUNT_VARIANT changes.
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
