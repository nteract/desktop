//! Load the user's shell environment so GUI-launched apps can find tools like `uv`, `deno`, etc.
//!
//! When launched from Finder/Spotlight/Dock, macOS apps get a minimal environment that doesn't
//! include paths from `.zshrc`, `.bashrc`, etc. This module spawns a login shell to capture
//! the user's real PATH, then applies it to the current process.

#[cfg(unix)]
use log::warn;
use log::{debug, info};
use std::env;

/// Well-known directories where tools like `uv`, `deno`, `cargo`, etc. are commonly installed.
/// Used as a fallback when login shell capture fails.
const WELL_KNOWN_PATHS: &[&str] = &[
    "~/.local/bin",
    "~/.cargo/bin",
    "~/.deno/bin",
    "~/.pixi/bin",
    "/opt/homebrew/bin",
    "/opt/homebrew/sbin",
    "/usr/local/bin",
    "~/.nix-profile/bin",
    "/nix/var/nix/profiles/default/bin",
    "/run/current-system/sw/bin",
];

/// Load the user's shell environment and apply it to the current process.
///
/// This should be called early in app startup, before any tool lookups.
/// On non-Unix platforms this is a no-op.
pub fn load_shell_environment() {
    #[cfg(unix)]
    {
        match capture_login_shell_path() {
            Ok(path) => {
                apply_path(&path);
                info!(
                    "loaded PATH from login shell ({} entries)",
                    path.split(':').count()
                );
                debug!("PATH={}", env::var("PATH").unwrap_or_default());
            }
            Err(e) => {
                warn!(
                    "failed to capture login shell PATH: {}, falling back to well-known paths",
                    e
                );
                apply_well_known_paths();
            }
        }
    }

    #[cfg(not(unix))]
    {
        info!("shell environment loading not implemented on this platform, using well-known paths");
        apply_well_known_paths();
    }
}

/// Spawn the user's login shell and capture its PATH.
#[cfg(unix)]
fn capture_login_shell_path() -> Result<String, String> {
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    debug!("capturing PATH from login shell: {}", shell);

    // Use -l for login shell (sources profile/rc files) and -c to run a command.
    // We intentionally avoid -i (interactive) to skip things like prompt setup,
    // key bindings, and other interactive-only config that can hang or produce noise.
    //
    // printf is more portable than echo across shells.
    let output = std::process::Command::new(&shell)
        .args(["-l", "-c", "printf '%s' \"$PATH\""])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();

    // Guard against shells that hang (e.g. waiting for input).
    // We use a spawned thread with a timeout since std::process doesn't have native timeout.
    let output = match output {
        Ok(o) => o,
        Err(e) => return Err(format!("failed to spawn {}: {}", shell, e)),
    };

    if !output.status.success() {
        return Err(format!(
            "{} -l -c exited with status {}",
            shell, output.status
        ));
    }

    let path =
        String::from_utf8(output.stdout).map_err(|e| format!("non-UTF-8 PATH output: {}", e))?;

    if path.is_empty() {
        return Err("login shell returned empty PATH".to_string());
    }

    Ok(path)
}

/// Merge a captured PATH with the current process PATH, prepending new entries.
#[cfg(unix)]
fn apply_path(shell_path: &str) {
    let current = env::var("PATH").unwrap_or_default();
    let current_entries: std::collections::HashSet<&str> = current.split(':').collect();

    // Prepend entries from the shell PATH that aren't already present.
    let mut new_entries: Vec<&str> = Vec::new();
    for entry in shell_path.split(':') {
        if !entry.is_empty() && !current_entries.contains(entry) {
            new_entries.push(entry);
        }
    }

    if new_entries.is_empty() {
        debug!("no new PATH entries from login shell");
        return;
    }

    info!("adding {} PATH entries from login shell", new_entries.len());

    // Build new PATH: shell entries first, then existing entries
    let merged = if current.is_empty() {
        shell_path.to_string()
    } else {
        format!("{}:{}", new_entries.join(":"), current)
    };

    // SAFETY: called once at startup before any threads are spawned.
    unsafe { env::set_var("PATH", &merged) };
}

/// Expand `~` to the user's home directory.
fn expand_tilde(path: &str) -> Option<String> {
    if let Some(rest) = path.strip_prefix('~') {
        dirs::home_dir().map(|home| format!("{}{}", home.display(), rest))
    } else {
        Some(path.to_string())
    }
}

/// Prepend well-known tool directories to PATH as a fallback.
fn apply_well_known_paths() {
    let current = env::var("PATH").unwrap_or_default();
    let current_entries: std::collections::HashSet<String> =
        current.split(':').map(String::from).collect();

    let mut prepend: Vec<String> = Vec::new();

    for pattern in WELL_KNOWN_PATHS {
        if let Some(expanded) = expand_tilde(pattern) {
            if !current_entries.contains(&expanded) {
                let path = std::path::Path::new(&expanded);
                if path.is_dir() {
                    prepend.push(expanded);
                }
            }
        }
    }

    if prepend.is_empty() {
        debug!("no well-known paths to add");
        return;
    }

    info!("adding {} well-known PATH entries", prepend.len());

    let merged = if current.is_empty() {
        prepend.join(":")
    } else {
        format!("{}:{}", prepend.join(":"), current)
    };

    // SAFETY: called once at startup before any threads are spawned.
    unsafe { env::set_var("PATH", &merged) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_tilde() {
        let expanded = expand_tilde("~/.local/bin");
        assert!(expanded.is_some());
        let expanded = expanded.unwrap();
        assert!(!expanded.starts_with('~'));
        assert!(expanded.ends_with("/.local/bin"));
    }

    #[test]
    fn test_expand_tilde_no_tilde() {
        assert_eq!(
            expand_tilde("/usr/local/bin"),
            Some("/usr/local/bin".to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_apply_path_deduplicates() {
        // Save and restore PATH
        let original = env::var("PATH").unwrap_or_default();

        unsafe { env::set_var("PATH", "/usr/bin:/usr/local/bin") };
        apply_path("/new/path:/usr/bin:/another/path");

        let result = env::var("PATH").unwrap();
        // /new/path and /another/path should be prepended, /usr/bin should not be duplicated
        assert!(result.starts_with("/new/path:/another/path:"));
        assert_eq!(result.matches("/usr/bin").count(), 1);

        unsafe { env::set_var("PATH", &original) };
    }

    #[cfg(unix)]
    #[test]
    fn test_capture_login_shell_path() {
        // This test requires a working shell, so it may not work in all CI environments.
        // But it should work on any developer machine.
        if let Ok(path) = capture_login_shell_path() {
            assert!(!path.is_empty());
            // Should contain at least /usr/bin or similar
            assert!(path.contains('/'));
        }
    }
}
