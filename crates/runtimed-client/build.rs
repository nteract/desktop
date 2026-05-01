use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    build_metadata::emit_git_rerun_hints();
    build_metadata::write_git_hash(&out_dir);
}
