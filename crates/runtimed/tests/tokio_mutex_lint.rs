//! CI lint: ensure no tokio::sync::Mutex guards are held across .await points.
//!
//! Uses the async-rust-lsp rule engine (tree-sitter based) to scan all runtimed
//! source files. Any violation is a hard CI failure.
//!
//! Reached zero violations on 2026-04-08 after:
//! - #1614, #1637, #1638: initial fixes and lint expansion
//! - #1642: Phase 1 mechanical burndown (58 → 19)
//! - #1647: Kernel actor pattern (19 → 14)
//! - Dead code removal + IOPub scoping (14 → 0)

#[test]
fn runtimed_has_no_tokio_mutex_across_await() {
    let src_dir = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));

    let rs_files: Vec<std::path::PathBuf> = std::fs::read_dir(&src_dir)
        .unwrap_or_else(|e| panic!("failed to read src dir: {e}"))
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension().is_some_and(|ext| ext == "rs") {
                Some(path)
            } else {
                None
            }
        })
        .collect();

    assert!(
        !rs_files.is_empty(),
        "no .rs files found in {}",
        src_dir.display()
    );

    let mut violations = Vec::new();

    for path in &rs_files {
        let source = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));

        let diagnostics =
            async_rust_lsp::rules::mutex_across_await::check_mutex_across_await(&source);

        let file_name = path.file_name().map_or_else(
            || panic!("no file name for {}", path.display()),
            |n| n.to_string_lossy().to_string(),
        );

        for d in diagnostics {
            violations.push(format!(
                "  {}:{}: {}",
                file_name,
                d.range.start.line + 1,
                d.message
            ));
        }
    }

    if !violations.is_empty() {
        let mut msg = format!(
            "Found {} tokio Mutex guard(s) held across .await in runtimed sources:\n\n",
            violations.len()
        );
        for v in &violations {
            msg.push_str(v);
            msg.push('\n');
        }
        msg.push_str(
            "\nFix: scope each lock in its own block so the guard drops before the next .await.\n\
             See: https://github.com/nteract/desktop/pull/1614\n",
        );
        panic!("{msg}");
    }
}
