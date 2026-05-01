use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const GIT_HASH_ENV: &str = "NTERACT_BUILD_GIT_HASH";
const GIT_DATE_ENV: &str = "NTERACT_BUILD_GIT_DATE";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitMetadata {
    pub hash: String,
    pub branch: String,
    pub date: String,
}

pub fn emit_git_rerun_hints() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed={GIT_HASH_ENV}");
    println!("cargo:rerun-if-env-changed={GIT_DATE_ENV}");

    if let Some(path) = git_path("HEAD") {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    if let Some(ref_name) = git_output(&["rev-parse", "--symbolic-full-name", "HEAD"]) {
        if ref_name != "HEAD" {
            if let Some(path) = git_path(&ref_name) {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }

    if let Some(path) = git_path("packed-refs") {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

pub fn collect_git_metadata() -> GitMetadata {
    GitMetadata {
        hash: git_hash(),
        branch: git_output(&["rev-parse", "--abbrev-ref", "HEAD"])
            .unwrap_or_else(|| "unknown".to_string()),
        date: git_date(),
    }
}

pub fn write_git_hash(out_dir: &Path) {
    let hash = git_hash();
    write_if_changed(&out_dir.join("git_hash.txt"), &hash);
}

pub fn write_git_metadata(out_dir: &Path) {
    let metadata = collect_git_metadata();
    write_if_changed(&out_dir.join("git_hash.txt"), &metadata.hash);
    write_if_changed(&out_dir.join("git_branch.txt"), &metadata.branch);
    write_if_changed(&out_dir.join("git_date.txt"), &metadata.date);
}

pub fn normalized_short_hash(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let without_dirty = trimmed.strip_suffix("+dirty").unwrap_or(trimmed);
    let candidate = without_dirty
        .split_once('+')
        .map_or(without_dirty, |(_, hash)| hash);
    if candidate.is_empty() {
        return None;
    }

    let hex_len = candidate
        .bytes()
        .take_while(|byte| byte.is_ascii_hexdigit())
        .count();

    if hex_len >= 7 {
        Some(candidate[..7].to_string())
    } else if hex_len == candidate.len() {
        Some(candidate.to_string())
    } else {
        None
    }
}

fn git_hash() -> String {
    env::var(GIT_HASH_ENV)
        .ok()
        .and_then(|value| normalized_short_hash(&value))
        .or_else(|| git_output(&["rev-parse", "--short=7", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_string())
}

fn git_date() -> String {
    env::var(GIT_DATE_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| git_output(&["show", "-s", "--format=%cs", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_string())
}

fn git_path(path: &str) -> Option<PathBuf> {
    git_output(&["rev-parse", "--path-format=absolute", "--git-path", path]).map(PathBuf::from)
}

fn git_output(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|stdout| stdout.trim().to_string())
        .filter(|stdout| !stdout.is_empty())
}

fn write_if_changed(path: &Path, content: &str) {
    let needs_write = match fs::read_to_string(path) {
        Ok(existing) => existing != content,
        Err(_) => true,
    };

    if needs_write {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(path, content).unwrap_or_else(|err| {
            panic!(
                "failed to write build metadata to {}: {err}",
                path.display()
            )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::normalized_short_hash;

    #[test]
    fn full_sha_shortens_to_seven_chars() {
        assert_eq!(
            normalized_short_hash("20a81f6098a0669071b94a66f3842ffac08da508").as_deref(),
            Some("20a81f6")
        );
    }

    #[test]
    fn short_sha_is_preserved() {
        assert_eq!(normalized_short_hash("abc1234").as_deref(), Some("abc1234"));
    }

    #[test]
    fn very_short_hex_sha_is_preserved() {
        assert_eq!(normalized_short_hash("abc123").as_deref(), Some("abc123"));
    }

    #[test]
    fn empty_hash_is_unknown_to_caller() {
        assert_eq!(normalized_short_hash("  "), None);
    }

    #[test]
    fn non_hex_hash_is_unknown_to_caller() {
        assert_eq!(normalized_short_hash("not-hex-at-all"), None);
    }

    #[test]
    fn dirty_only_hash_is_unknown_to_caller() {
        assert_eq!(normalized_short_hash("+dirty"), None);
    }

    #[test]
    fn dirty_suffix_is_ignored_for_build_identity() {
        assert_eq!(
            normalized_short_hash("2.3.5+20a81f6+dirty").as_deref(),
            Some("20a81f6")
        );
        assert_eq!(
            normalized_short_hash("20a81f6098a0669071b94a66f3842ffac08da508+dirty").as_deref(),
            Some("20a81f6")
        );
    }
}
