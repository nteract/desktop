use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    write_git_hash();

    // No rerun-if-changed directives — cargo's default behavior reruns
    // this script when any file in the package changes, which is exactly
    // when we want a fresh commit hash check.
}

/// Write the git commit hash to `$OUT_DIR/git_hash.txt`, skipping the write
/// if the content hasn't changed. See `crates/runtimed/build.rs` for the
/// rationale — this avoids recompilation when the hash doesn't change.
#[allow(clippy::unwrap_used)]
fn write_git_hash() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let hash_file = out_dir.join("git_hash.txt");
    let hash = git_commit_short();

    let needs_write = match fs::read_to_string(&hash_file) {
        Ok(existing) => existing != hash,
        Err(_) => true,
    };

    if needs_write {
        fs::write(&hash_file, &hash).unwrap();
    }
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
