//! CI lint: ensure no tokio::sync::Mutex guards are held across .await points.
//!
//! Uses the async-rust-lsp rule engine (tree-sitter based) to scan all runtimed
//! source files for the pattern that caused the convoy deadlock fixed in PR #1614.

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

    let mut all_violations = Vec::new();

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
            all_violations.push(format!(
                "  {}:{}: {}",
                file_name,
                d.range.start.line + 1,
                d.message
            ));
        }
    }

    if !all_violations.is_empty() {
        let mut msg =
            String::from("Found tokio Mutex guard(s) held across .await in runtimed sources:\n\n");
        for v in &all_violations {
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
