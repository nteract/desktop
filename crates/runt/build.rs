use std::path::PathBuf;

fn main() {
    let variant = std::env::var("RUNT_VARIANT").unwrap_or_default();
    println!("cargo:rustc-env=RUNT_VARIANT={variant}");
    println!("cargo:rerun-if-env-changed=RUNT_VARIANT");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    build_metadata::emit_git_rerun_hints();
    build_metadata::write_git_hash(&out_dir);
}
