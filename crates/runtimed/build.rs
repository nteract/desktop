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

    // Capture short commit hash for version-mismatch detection.
    // This ensures the daemon gets restarted when the binary changes,
    // even if the crate version (Cargo.toml) hasn't been bumped.
    let commit = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=GIT_COMMIT={}", commit);

    // Use `git rev-parse --git-dir` to find the actual git metadata directory.
    // In a worktree, `.git` is a file pointing elsewhere (e.g.
    // `../../.git/worktrees/<name>`), so hard-coding `../../.git/HEAD` would
    // watch a path that doesn't exist and Cargo would never re-run this script.
    let git_dir = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string());

    if let Some(git_dir) = git_dir {
        // Re-run if git HEAD changes (detects branch switches).
        println!("cargo:rerun-if-changed={}/HEAD", git_dir);

        // Also track the ref that HEAD points to (detects new commits on the
        // current branch). When HEAD is "ref: refs/heads/main", new commits
        // update refs/heads/main but NOT HEAD itself.
        if let Ok(head) = std::fs::read_to_string(format!("{}/HEAD", git_dir)) {
            let head = head.trim();
            if let Some(refpath) = head.strip_prefix("ref: ") {
                // The ref itself may live in the common git dir (for worktrees,
                // that's the parent repo's .git), so check both locations.
                let common_dir = Command::new("git")
                    .args(["rev-parse", "--git-common-dir"])
                    .output()
                    .ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|| git_dir.clone());

                println!("cargo:rerun-if-changed={}/{}", git_dir, refpath);
                if common_dir != git_dir {
                    println!("cargo:rerun-if-changed={}/{}", common_dir, refpath);
                }

                // Packed-refs is updated when git packs loose refs or during
                // fetch/gc. A ref might only exist here, so track it too.
                println!("cargo:rerun-if-changed={}/packed-refs", common_dir);
            }
        }
    }
}
