//! CI lint: ensure no tokio::sync::Mutex guards are held across .await points.
//!
//! Uses the async-rust-lsp rule engine (tree-sitter based) to scan all runtimed
//! source files for the pattern that caused the convoy deadlock fixed in PR #1614.
//!
//! All violations are currently advisory (reported but don't fail CI) to support
//! incremental burn-down after upgrading to async-rust-lsp v0.2.0 which has
//! significantly improved detection (let-binding RHS, nested blocks).
//!
//! To gate a file (make violations a hard CI failure), add it to GATED_FILES.
//! Do this once the file is fully clean.

/// Files that have been fully cleaned of mutex-across-await violations.
/// Adding a file here makes violations in it a hard CI failure.
const GATED_FILES: &[&str] = &["daemon.rs", "notebook_sync_server.rs", "sync_server.rs"];

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

    let mut gated_violations = Vec::new();
    let mut advisory_violations = Vec::new();

    for path in &rs_files {
        let source = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));

        let diagnostics =
            async_rust_lsp::rules::mutex_across_await::check_mutex_across_await(&source);

        let file_name = path.file_name().map_or_else(
            || panic!("no file name for {}", path.display()),
            |n| n.to_string_lossy().to_string(),
        );

        let is_gated = GATED_FILES.contains(&file_name.as_str());

        for d in diagnostics {
            let line = format!("  {}:{}: {}", file_name, d.range.start.line + 1, d.message);
            if is_gated {
                gated_violations.push(line);
            } else {
                advisory_violations.push(line);
            }
        }
    }

    // Report advisory violations as warnings (don't fail)
    if !advisory_violations.is_empty() {
        eprintln!(
            "\n\u{26a0} {} mutex-across-await violation(s) in runtimed sources:\n",
            advisory_violations.len()
        );
        for v in &advisory_violations {
            eprintln!("{v}");
        }
        eprintln!(
            "\nThese are reported for burn-down tracking but do not fail CI.\n\
             To gate a file, add it to GATED_FILES in tokio_mutex_lint.rs.\n"
        );
    }

    // Fail on gated file violations
    if !gated_violations.is_empty() {
        let mut msg =
            String::from("Found tokio Mutex guard(s) held across .await in gated files:\n\n");
        for v in &gated_violations {
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
