use std::path::PathBuf;

fn main() {
    let out_dir = out_dir();
    build_metadata::emit_git_rerun_hints();
    build_metadata::write_git_hash(&out_dir);
}

fn out_dir() -> PathBuf {
    match std::env::var("OUT_DIR") {
        Ok(value) => PathBuf::from(value),
        Err(err) => panic!("OUT_DIR is required for build metadata: {err}"),
    }
}
