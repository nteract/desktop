use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let variant = std::env::var("RUNT_VARIANT").unwrap_or_default();
    println!("cargo:rustc-env=RUNT_VARIANT={variant}");
    println!("cargo:rerun-if-env-changed=RUNT_VARIANT");

    // Also rerun when this build script changes (since we emit
    // rerun-if-changed above, cargo won't use its default behavior).
    println!("cargo:rerun-if-changed=build.rs");

    write_git_hash();
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
