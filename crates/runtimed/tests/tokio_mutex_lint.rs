//! CI lint: ensure no tokio::sync::Mutex guards are held across .await points.
//!
//! Uses the async-rust-lsp rule engine (tree-sitter based) to scan daemon.rs
//! for the pattern that caused the convoy deadlock fixed in PR #1614.

#[test]
fn daemon_has_no_tokio_mutex_across_await() {
    let daemon_source =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/daemon.rs"))
            .expect("failed to read daemon.rs");

    let diagnostics =
        async_rust_lsp::rules::mutex_across_await::check_mutex_across_await(&daemon_source);

    if !diagnostics.is_empty() {
        let mut msg =
            String::from("Found tokio Mutex guard(s) held across .await in daemon.rs:\n\n");
        for d in &diagnostics {
            msg.push_str(&format!(
                "  line {}: {}\n",
                d.range.start.line + 1,
                d.message
            ));
        }
        msg.push_str(
            "\nFix: clone the Arc in a scoped block, drop the lock, then await.\n\
             See: https://github.com/nteract/desktop/pull/1614\n",
        );
        panic!("{msg}");
    }
}
