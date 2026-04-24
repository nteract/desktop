use std::fs;
use std::path::PathBuf;
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

    // Re-run whenever the current HEAD moves so the embedded git hash
    // reflects the tree actually being compiled. Without this, a `git
    // checkout` or merge leaves cargo happy to reuse a stale OUT_DIR
    // whose `git_hash.txt` points at a previous commit — the daemon
    // then mis-reports its version (e.g. 7583085 after landing bf42cea)
    // and anyone reading the status thinks the binary is out of date.
    //
    // `.git/HEAD` covers branch switches. `.git/refs/heads/<branch>`
    // covers new commits on the current branch; we tell cargo to watch
    // the packed-refs file too for the common "refs compacted" case.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
    println!("cargo:rerun-if-changed=../../.git/packed-refs");

    write_git_hash();
}

/// Write the git commit hash to `$OUT_DIR/git_hash.txt`, skipping the write
/// if the content hasn't changed. Consumer code uses:
///
///     include_str!(concat!(env!("OUT_DIR"), "/git_hash.txt"))
///
/// Because cargo tracks file modification times on included files, an
/// unchanged hash file means no recompilation — even after a rebase that
/// doesn't touch this crate's source. This replaces the old
/// `cargo:rustc-env=GIT_COMMIT=...` approach which forced recompilation
/// every time the build script ran.
#[allow(clippy::unwrap_used)]
fn write_git_hash() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let hash_file = out_dir.join("git_hash.txt");
    let hash = git_commit_short();

    // Only write if the content actually changed — preserves mtime so
    // cargo's incremental compilation skips dependent rustc invocations.
    let needs_write = match fs::read_to_string(&hash_file) {
        Ok(existing) => existing != hash,
        Err(_) => true,
    };

    if needs_write {
        fs::write(&hash_file, &hash).unwrap();
    }
}

fn git_commit_short() -> String {
    let hash = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    if hash == "unknown" {
        return hash;
    }

    if git_worktree_is_dirty() {
        format!("{hash}+dirty")
    } else {
        hash
    }
}

/// Returns true if `git status --porcelain` reports any uncommitted changes.
/// A binary built from a dirty worktree gets its embedded hash marked
/// `<sha>+dirty` so the running version honestly identifies what was
/// compiled in. Returns false on any git failure (no repo, no git
/// binary) so the absence of git doesn't lie about cleanliness.
fn git_worktree_is_dirty() -> bool {
    Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}
