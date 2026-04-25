#![allow(clippy::unwrap_used, clippy::expect_used)]

//! CI lint: ensure no cancel-unsafe tokio I/O futures appear in the
//! future-expression position of `tokio::select!` arms across any
//! workspace crate.
//!
//! Cancel-unsafe futures (`read_exact`, `write_all`, `read_line`, …)
//! discard buffered bytes when dropped. When a sibling arm wins the
//! `select!` race, the losing future is dropped — the next read starts
//! in the wrong place and length-prefixed protocols silently desync.
//! See PR #2182 for the relay desync fix and rgbkrk/async-rust-lsp#4
//! for the rule.
//!
//! Project-local wrappers (e.g. `recv_typed_frame` that delegates to
//! `read_exact`) are listed in `.async-rust-lsp.toml` at the workspace
//! root and added to the rule's blocklist via the extras config.
//!
//! Scope: every `crates/<name>/src/` tree, walked recursively. New
//! workspace members are picked up automatically.

use async_rust_lsp::config::Config;
use async_rust_lsp::rules::cancel_unsafe_in_select::check_cancel_unsafe_in_select_with;
use std::path::{Path, PathBuf};

#[test]
fn workspace_has_no_cancel_unsafe_in_select() {
    let workspace_root = workspace_root();
    let crates_dir = workspace_root.join("crates");

    // Load project-local extras from .async-rust-lsp.toml at the
    // workspace root. If the file is absent or malformed, this falls
    // back to defaults (built-in tokio-primitives only).
    let (config, _) = Config::discover_from(&workspace_root);
    let extras = &config.rules.cancel_unsafe_in_select.extra;

    let mut violations: Vec<String> = Vec::new();
    let mut file_count: usize = 0;

    for entry in std::fs::read_dir(&crates_dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", crates_dir.display()))
    {
        let crate_dir = match entry {
            Ok(e) => e.path(),
            Err(_) => continue,
        };
        if !crate_dir.is_dir() {
            continue;
        }
        let src_dir = crate_dir.join("src");
        if !src_dir.is_dir() {
            continue;
        }

        walk_rs_files(&src_dir, &mut |path| {
            file_count += 1;
            let source = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));

            for d in check_cancel_unsafe_in_select_with(&source, extras) {
                let rel = path.strip_prefix(&workspace_root).unwrap_or(path);
                let first_line = d.message.lines().next().unwrap_or(d.message.as_str());
                violations.push(format!(
                    "  {}:{}: {}",
                    rel.display(),
                    d.range.start.line + 1,
                    first_line
                ));
            }
        });
    }

    assert!(
        file_count > 0,
        "no .rs files scanned under {}",
        crates_dir.display()
    );

    if !violations.is_empty() {
        let mut msg = format!(
            "Found {} cancel-unsafe future(s) in tokio::select! arms across {} files:\n\n",
            violations.len(),
            file_count
        );
        for v in &violations {
            msg.push_str(v);
            msg.push('\n');
        }
        msg.push_str(
            "\nFix: move the call into a dedicated reader/writer task that forwards parsed \
             messages over an mpsc channel; then `select!` on `channel.recv()`, which is \
             cancel-safe. See nteract/desktop#2182 for the FramedReader actor pattern.\n",
        );
        panic!("{msg}");
    }
}

/// Walk up two parents from `crates/runtimed` to the workspace root.
fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .unwrap_or(manifest)
}

fn walk_rs_files(dir: &Path, cb: &mut dyn FnMut(&Path)) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_rs_files(&path, cb);
        } else if path.extension().is_some_and(|e| e == "rs") {
            cb(&path);
        }
    }
}
