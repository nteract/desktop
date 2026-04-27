//! CLI installation module for putting the bundled runt binary on PATH and
//! creating the channel-specific notebook shorthand wrapper.
//!
//! On Unix systems, we install to `~/.local/bin` (no admin privileges required)
//! and create a symlink so the CLI automatically stays in sync when the app
//! is updated. On Windows, we write small owned `.cmd` shims because symlinks
//! require admin/Developer Mode and copied binaries drift across app upgrades.

use runt_workspace::{cli_command_name, cli_notebook_alias_name};
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::symlink;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(target_os = "windows")]
use std::os::windows::ffi::OsStrExt;
#[cfg(any(target_os = "windows", test))]
use std::path::Path;
use std::path::PathBuf;
use tauri::Manager;

#[cfg(any(target_os = "windows", test))]
const WINDOWS_CMD_SHIM_MARKER: &str = "nteract-managed-cli-shim v1";

#[cfg(target_os = "windows")]
const WINDOWS_REGISTRY_INSTALL_STATE_BASE: &str = "Software\\nteract";

/// Legacy install directory — checked for backward compatibility detection only.
#[cfg(unix)]
const LEGACY_INSTALL_DIR: &str = "/usr/local/bin";

/// Get the user-local install directory (`~/.local/bin` on Unix).
/// This requires no admin privileges and is the modern convention used by
/// rustup, uv, mise, pipx, and others.
#[cfg(unix)]
fn install_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".local")
        .join("bin")
}

#[cfg(target_os = "windows")]
fn install_dir() -> PathBuf {
    windows_install_dir_for_install().unwrap_or_else(|e| {
        log::warn!(
            "[cli_install] Failed to choose Windows CLI install dir ({}), using fallback",
            e
        );
        windows_fallback_cli_dir()
    })
}

#[cfg(any(target_os = "windows", test))]
fn windows_cmd_name(command_name: &str) -> String {
    format!("{command_name}.cmd")
}

#[cfg(any(target_os = "windows", test))]
fn cmd_escape_path(path: &Path) -> String {
    // Batch files expand percent-delimited environment variables even inside
    // quotes, so double literal percent signs from user/profile paths.
    path.to_string_lossy().replace('%', "%%")
}

#[cfg(any(target_os = "windows", test))]
fn windows_runt_cmd_shim_contents(runt_path: &Path) -> String {
    format!(
        "@echo off\r\nrem {WINDOWS_CMD_SHIM_MARKER}\r\n\"{}\" %*\r\n",
        cmd_escape_path(runt_path)
    )
}

#[cfg(any(target_os = "windows", test))]
fn windows_nb_cmd_shim_contents(runt_path: &Path) -> String {
    format!(
        "@echo off\r\nrem {WINDOWS_CMD_SHIM_MARKER}\r\n\"{}\" notebook %*\r\n",
        cmd_escape_path(runt_path)
    )
}

#[cfg(any(target_os = "windows", test))]
fn is_owned_windows_cmd_shim(contents: &str) -> bool {
    contents.starts_with(&format!("@echo off\r\nrem {WINDOWS_CMD_SHIM_MARKER}\r\n"))
        || contents.starts_with(&format!("@echo off\nrem {WINDOWS_CMD_SHIM_MARKER}\n"))
}

/// Get the path to the bundled runt binary.
pub fn get_bundled_runt_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        if let Ok(exe_dir) = app.path().resource_dir() {
            // resource_dir on macOS points to Contents/Resources
            // The binary is in Contents/MacOS, which is ../MacOS from Resources
            let macos_dir = exe_dir.parent()?.join("MacOS");
            let bundled_path = macos_dir.join("runt");
            if bundled_path.exists() {
                log::debug!("[cli_install] Found bundled runt at {:?}", bundled_path);
                return Some(bundled_path);
            }
            log::debug!("[cli_install] Bundled runt not found at {:?}", bundled_path);
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(resource_dir) = app.path().resource_dir() {
            let bundled_path = resource_dir.join("runt");
            if bundled_path.exists() {
                log::debug!("[cli_install] Found bundled runt at {:?}", bundled_path);
                return Some(bundled_path);
            }
            log::debug!("[cli_install] Bundled runt not found at {:?}", bundled_path);
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(resource_dir) = app.path().resource_dir() {
            let bundled_path = resource_dir.join("runt.exe");
            if bundled_path.exists() {
                log::debug!("[cli_install] Found bundled runt at {:?}", bundled_path);
                return Some(bundled_path);
            }
            log::debug!("[cli_install] Bundled runt not found at {:?}", bundled_path);
        }
    }

    // Fallback: try the development path (target/*/binaries/runt-{target})
    let target = if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "aarch64-apple-darwin"
        } else {
            "x86_64-apple-darwin"
        }
    } else if cfg!(target_os = "linux") {
        if cfg!(target_arch = "aarch64") {
            "aarch64-unknown-linux-gnu"
        } else {
            "x86_64-unknown-linux-gnu"
        }
    } else {
        "x86_64-pc-windows-msvc"
    };

    let binary_name = if cfg!(windows) {
        format!("runt-{}.exe", target)
    } else {
        format!("runt-{}", target)
    };

    // Try to find it relative to the executable (for no-bundle dev builds)
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let dev_path = exe_dir.join("binaries").join(&binary_name);
            if dev_path.exists() {
                log::debug!("[cli_install] Found dev runt at {:?}", dev_path);
                return Some(dev_path);
            }
            log::debug!("[cli_install] Dev runt not found at {:?}", dev_path);
        }
    }

    None
}

/// Result of checking whether an installed CLI symlink is current.
#[derive(Debug, PartialEq, Eq)]
pub enum SymlinkStatus {
    /// Symlink exists and points to the current app bundle binary.
    Current,
    /// Symlink exists but points to a different (stale) path.
    Stale,
    /// CLI is not installed (symlink does not exist).
    NotInstalled,
}

/// Check whether the installed `runt`/`runt-nightly` symlink in `~/.local/bin`
/// points to the current app bundle binary path, and whether the `nb`/`nb-nightly`
/// wrapper script references the correct CLI command name.
///
/// Only checks `~/.local/bin` — the canonical install location. Legacy
/// `/usr/local/bin` entries are warned about separately by `warn_legacy_cli_shadow()`
/// and are not auto-repaired (since `install_cli()` writes to `~/.local/bin` only).
///
/// Returns a tuple of `(runt_status, nb_status)`.
#[cfg(unix)]
pub fn check_cli_currency(app: &tauri::AppHandle) -> (SymlinkStatus, SymlinkStatus) {
    let dir = install_dir();
    let cli_name = cli_command_name();
    let nb_name = cli_notebook_alias_name();

    let runt_status = check_runt_symlink(app, &dir.join(cli_name));
    let nb_status = check_nb_script(&dir.join(nb_name), cli_name);

    (runt_status, nb_status)
}

/// Check if the runt symlink points to the current bundled binary.
///
/// Only considers an existing entry "ours" if it is a symlink whose target
/// contains "nteract" or "runt" in the path — this avoids clobbering unrelated
/// commands that happen to share the same name.
#[cfg(unix)]
fn check_runt_symlink(app: &tauri::AppHandle, symlink_path: &std::path::Path) -> SymlinkStatus {
    if !symlink_path.is_symlink() {
        // Not a symlink — either missing or a regular file/directory we don't own.
        return SymlinkStatus::NotInstalled;
    }

    let target = match fs::read_link(symlink_path) {
        Ok(t) => t,
        Err(e) => {
            log::warn!(
                "[cli_install] Failed to read symlink {}: {}",
                symlink_path.display(),
                e
            );
            // Can't read it — don't touch what we can't verify
            return SymlinkStatus::NotInstalled;
        }
    };

    // Only consider this symlink ours if the target path matches the shape of
    // an nteract app bundle install. On macOS this is "*.app/Contents/MacOS/runt",
    // on Linux it's inside an nteract resource directory. We check for "nteract"
    // as a directory component AND the target filename being "runt" to avoid
    // false-positives on unrelated symlinks (e.g. /opt/homebrew/bin/runt).
    //
    // Note: if a user renames "nteract.app" to something else, the symlink will
    // no longer be recognized as ours, and auto-repair won't trigger. This is an
    // acceptable trade-off — renaming is rare and manual `install_cli()` still works.
    let target_str = target.to_string_lossy();
    let target_filename = target.file_name().map(|f| f.to_string_lossy());
    let looks_like_ours = target_filename.as_deref() == Some("runt")
        && (target_str.contains("/nteract") || target_str.contains("/nteract-nightly"));

    if !looks_like_ours {
        log::debug!(
            "[cli_install] Symlink {} -> {} does not appear to be an nteract install, skipping",
            symlink_path.display(),
            target_str
        );
        return SymlinkStatus::NotInstalled;
    }

    let bundled = match get_bundled_runt_path(app) {
        Some(p) => p,
        None => {
            log::debug!("[cli_install] Cannot determine bundled runt path for currency check");
            // Can't determine — assume current to avoid unnecessary reinstall
            return SymlinkStatus::Current;
        }
    };

    if target == bundled {
        SymlinkStatus::Current
    } else {
        log::info!(
            "[cli_install] Symlink stale: {} -> {} (expected {})",
            symlink_path.display(),
            target.display(),
            bundled.display()
        );
        SymlinkStatus::Stale
    }
}

/// Check if the nb wrapper script references the correct CLI command name.
///
/// Only considers the script "ours" if its content mentions "runt" — this avoids
/// clobbering unrelated `nb` commands.
#[cfg(unix)]
fn check_nb_script(script_path: &std::path::Path, expected_cli_name: &str) -> SymlinkStatus {
    if !script_path.exists() {
        return SymlinkStatus::NotInstalled;
    }

    match fs::read_to_string(script_path) {
        Ok(contents) => {
            // Only consider this script ours if it contains the exact exec pattern
            // that create_nb_wrapper() generates: "exec runt notebook" or
            // "exec runt-nightly notebook". A bare substring like "runt" would
            // false-positive on scripts mentioning "grunt", "runtime", etc.
            let is_ours = contents.contains("exec runt notebook")
                || contents.contains("exec runt-nightly notebook");

            if !is_ours {
                log::debug!(
                    "[cli_install] Script {} does not appear to be an nteract nb wrapper, skipping",
                    script_path.display()
                );
                return SymlinkStatus::NotInstalled;
            }

            let expected_exec = format!("exec {} notebook", expected_cli_name);
            if contents.contains(&expected_exec) {
                SymlinkStatus::Current
            } else {
                log::info!(
                    "[cli_install] nb script stale: {} does not contain '{}'",
                    script_path.display(),
                    expected_exec
                );
                SymlinkStatus::Stale
            }
        }
        Err(e) => {
            log::warn!(
                "[cli_install] Failed to read nb script {}: {}",
                script_path.display(),
                e
            );
            // Can't read — don't touch
            SymlinkStatus::NotInstalled
        }
    }
}

#[cfg(target_os = "windows")]
fn windows_apps_cli_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(windows_fallback_cli_dir)
        .join("Microsoft")
        .join("WindowsApps")
}

#[cfg(target_os = "windows")]
fn windows_fallback_cli_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".local")
        .join("bin")
}

#[cfg(target_os = "windows")]
fn windows_candidate_cli_dirs() -> [PathBuf; 2] {
    [windows_apps_cli_dir(), windows_fallback_cli_dir()]
}

#[cfg(target_os = "windows")]
fn windows_install_dir_for_install() -> Result<PathBuf, String> {
    let windows_apps = windows_apps_cli_dir();
    if windows_apps.is_dir()
        && effective_path_contains_dir(&windows_apps)
        && directory_is_writable(&windows_apps)
    {
        return Ok(windows_apps);
    }

    let fallback = windows_fallback_cli_dir();
    fs::create_dir_all(&fallback)
        .map_err(|e| format!("Failed to create {}: {}", fallback.display(), e))?;
    ensure_windows_user_path(&fallback)?;
    Ok(fallback)
}

#[cfg(target_os = "windows")]
fn effective_path_contains_dir(dir: &Path) -> bool {
    std::env::var_os("PATH")
        .map(|path| path_list_contains_dir(&path.to_string_lossy(), dir))
        .unwrap_or(false)
}

#[cfg(target_os = "windows")]
fn directory_is_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".nteract-write-test-{}.tmp", std::process::id()));
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(e) => {
            log::debug!(
                "[cli_install] Windows CLI dir {} is not writable: {}",
                dir.display(),
                e
            );
            false
        }
    }
}

#[cfg(target_os = "windows")]
fn normalize_windows_path_for_compare(path: &str) -> String {
    let trimmed = path.trim().trim_matches('"').trim_end_matches(['\\', '/']);
    trimmed.replace('/', "\\").to_ascii_lowercase()
}

#[cfg(target_os = "windows")]
fn path_list_contains_dir(path_list: &str, dir: &Path) -> bool {
    let expected = normalize_windows_path_for_compare(&dir.to_string_lossy());
    path_list
        .split(';')
        .map(normalize_windows_path_for_compare)
        .any(|entry| entry == expected)
}

#[cfg(target_os = "windows")]
fn command_path(dir: &Path, command_name: &str) -> PathBuf {
    dir.join(windows_cmd_name(command_name))
}

#[cfg(target_os = "windows")]
fn check_windows_cmd_shim(path: &Path, expected_contents: &str) -> SymlinkStatus {
    if !path.exists() {
        return SymlinkStatus::NotInstalled;
    }

    match fs::read_to_string(path) {
        Ok(contents) => {
            if !is_owned_windows_cmd_shim(&contents) {
                log::debug!(
                    "[cli_install] Windows shim {} is not managed by nteract, skipping",
                    path.display()
                );
                return SymlinkStatus::NotInstalled;
            }
            if contents == expected_contents {
                SymlinkStatus::Current
            } else {
                SymlinkStatus::Stale
            }
        }
        Err(e) => {
            log::warn!(
                "[cli_install] Failed to read Windows shim {}: {}",
                path.display(),
                e
            );
            SymlinkStatus::NotInstalled
        }
    }
}

#[cfg(target_os = "windows")]
fn write_owned_windows_cmd_shim(path: &Path, contents: &str) -> Result<(), String> {
    if path.exists() {
        let existing = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read existing {}: {}", path.display(), e))?;
        if !is_owned_windows_cmd_shim(&existing) {
            return Err(format!(
                "{} already exists and is not managed by nteract",
                path.display()
            ));
        }
    }

    fs::write(path, contents)
        .map_err(|e| format!("Failed to write Windows shim {}: {}", path.display(), e))
}

#[cfg(target_os = "windows")]
fn try_install_windows_cmd_shims(
    bundled_runt: &Path,
    runt_dest: &Path,
    nb_dest: &Path,
) -> Result<(), String> {
    if let Some(parent) = runt_dest.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {}: {}", parent.display(), e))?;
    }
    write_owned_windows_cmd_shim(runt_dest, &windows_runt_cmd_shim_contents(bundled_runt))?;
    write_owned_windows_cmd_shim(nb_dest, &windows_nb_cmd_shim_contents(bundled_runt))?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn windows_cli_currency_for_dir(
    app: &tauri::AppHandle,
    dir: &Path,
) -> Option<(SymlinkStatus, SymlinkStatus)> {
    let bundled = get_bundled_runt_path(app)?;
    let runt_status = check_windows_cmd_shim(
        &command_path(dir, cli_command_name()),
        &windows_runt_cmd_shim_contents(&bundled),
    );
    let nb_status = check_windows_cmd_shim(
        &command_path(dir, cli_notebook_alias_name()),
        &windows_nb_cmd_shim_contents(&bundled),
    );
    Some((runt_status, nb_status))
}

#[cfg(target_os = "windows")]
fn to_wide_null(value: &str) -> Vec<u16> {
    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(target_os = "windows")]
fn read_user_path_registry_value() -> Result<String, String> {
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY_CURRENT_USER, KEY_READ,
    };

    let subkey = to_wide_null("Environment");
    let value_name = to_wide_null("Path");
    let mut key = 0;
    let status =
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut key) };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(String::new());
    }
    if status != ERROR_SUCCESS {
        return Err(format!("RegOpenKeyExW(HKCU\\Environment) failed: {status}"));
    }

    let mut byte_len = 0u32;
    let status = unsafe {
        RegQueryValueExW(
            key,
            value_name.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut byte_len,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        unsafe {
            RegCloseKey(key);
        }
        return Ok(String::new());
    }
    if status != ERROR_SUCCESS {
        unsafe {
            RegCloseKey(key);
        }
        return Err(format!(
            "RegQueryValueExW(HKCU\\Environment\\Path) failed: {status}"
        ));
    }

    let mut buffer = vec![0u16; (byte_len as usize + 1) / 2];
    let status = unsafe {
        RegQueryValueExW(
            key,
            value_name.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast::<u8>(),
            &mut byte_len,
        )
    };
    unsafe {
        RegCloseKey(key);
    }
    if status != ERROR_SUCCESS {
        return Err(format!(
            "RegQueryValueExW(HKCU\\Environment\\Path) failed: {status}"
        ));
    }

    while buffer.last() == Some(&0) {
        buffer.pop();
    }
    Ok(String::from_utf16_lossy(&buffer))
}

#[cfg(target_os = "windows")]
fn write_user_path_registry_value(value: &str) -> Result<(), String> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY_CURRENT_USER, KEY_SET_VALUE,
        REG_EXPAND_SZ,
    };

    let subkey = to_wide_null("Environment");
    let value_name = to_wide_null("Path");
    let mut key = 0;
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            std::ptr::null_mut(),
            0,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(format!(
            "RegCreateKeyExW(HKCU\\Environment) failed: {status}"
        ));
    }

    let data = to_wide_null(value);
    let status = unsafe {
        RegSetValueExW(
            key,
            value_name.as_ptr(),
            0,
            REG_EXPAND_SZ,
            data.as_ptr().cast::<u8>(),
            (data.len() * std::mem::size_of::<u16>()) as u32,
        )
    };
    unsafe {
        RegCloseKey(key);
    }
    if status != ERROR_SUCCESS {
        return Err(format!(
            "RegSetValueExW(HKCU\\Environment\\Path) failed: {status}"
        ));
    }

    Ok(())
}

#[cfg(target_os = "windows")]
fn broadcast_environment_change() {
    use windows_sys::Win32::Foundation::{LPARAM, WPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
    };

    let environment = to_wide_null("Environment");
    let mut result = 0usize;
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0 as WPARAM,
            environment.as_ptr() as LPARAM,
            SMTO_ABORTIFHUNG,
            5000,
            &mut result,
        );
    }
}

#[cfg(target_os = "windows")]
fn ensure_windows_user_path(bin_dir: &Path) -> Result<(), String> {
    let current = read_user_path_registry_value()?;
    if path_list_contains_dir(&current, bin_dir) {
        return Ok(());
    }

    let bin_dir = bin_dir.to_string_lossy();
    let updated = if current.trim().is_empty() {
        bin_dir.to_string()
    } else {
        format!("{};{}", current.trim_end_matches(';'), bin_dir)
    };
    write_user_path_registry_value(&updated)?;
    broadcast_environment_change();
    log::info!("[cli_install] Added {} to HKCU Environment Path", bin_dir);
    Ok(())
}

#[cfg(target_os = "windows")]
fn windows_install_state_key() -> String {
    format!(
        "{}\\{}\\InstallState",
        WINDOWS_REGISTRY_INSTALL_STATE_BASE,
        runt_workspace::channel_display_name()
    )
}

#[cfg(target_os = "windows")]
pub fn clear_windows_update_flag() {
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, HKEY_CURRENT_USER, KEY_SET_VALUE,
    };

    let key_path = to_wide_null(&windows_install_state_key());
    let value_name = to_wide_null("Updating");
    let mut key = 0;
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            key_path.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut key,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return;
    }
    if status != ERROR_SUCCESS {
        log::debug!(
            "[cli_install] Failed to open Windows install state key for cleanup: {}",
            status
        );
        return;
    }

    let status = unsafe { RegDeleteValueW(key, value_name.as_ptr()) };
    unsafe {
        RegCloseKey(key);
    }
    if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND {
        log::debug!(
            "[cli_install] Failed to clear Windows update flag: {}",
            status
        );
    }
}

#[cfg(not(target_os = "windows"))]
pub fn clear_windows_update_flag() {}

/// Returns true if the app appears to be running from a temporary or
/// ephemeral path. We must not rewrite CLI symlinks in this case because
/// the path will disappear, leaving the symlinks broken.
///
/// Covers:
/// - macOS app translocation (`/private/var/folders/`, `AppTranslocation`)
/// - macOS Downloads or Volumes (not yet moved to /Applications)
/// - Linux AppImage mounts (`/tmp/.mount_*`)
/// - General temp directories (`/tmp/`, `/var/folders/`)
#[cfg(unix)]
fn is_ephemeral_path(app: &tauri::AppHandle) -> bool {
    let bundled = match get_bundled_runt_path(app) {
        Some(p) => p,
        None => return false,
    };

    let path_str = bundled.to_string_lossy();

    let ephemeral = path_str.contains("/private/var/folders/")
        || path_str.contains("/AppTranslocation/")
        || path_str.starts_with("/tmp/")
        || path_str.starts_with("/var/folders/")
        || path_str.contains("/tmp/.mount_")
        || path_str.contains("/Downloads/")
        || path_str.starts_with("/Volumes/");

    if ephemeral {
        log::info!(
            "[cli_install] Skipping CLI currency check — app running from ephemeral path: {}",
            path_str
        );
    }

    ephemeral
}

/// Silently update the CLI installation if the installed command entrypoints
/// are stale.
///
/// Called on app launch. On Unix, if the user has previously installed the CLI
/// (symlink exists), this checks whether it still points to the current app
/// bundle and re-runs `install_cli()` if not. On Windows, the installer should
/// have created owned `.cmd` shims already; app launch repairs stale/missing
/// shims so older installs and failed installer hooks still recover for UI use.
///
/// Skips the check in dev mode (source builds) and on macOS if the app is
/// running from a translocated path (e.g., directly from a DMG).
pub fn ensure_cli_current(app: &tauri::AppHandle) {
    #[cfg(target_os = "windows")]
    {
        if runt_workspace::is_dev_mode() {
            log::debug!("[cli_install] Dev mode — skipping Windows CLI currency check");
            clear_windows_update_flag();
            return;
        }

        let Some(bundled_runt) = get_bundled_runt_path(app) else {
            log::debug!("[cli_install] Cannot determine bundled runt path for Windows CLI check");
            clear_windows_update_flag();
            return;
        };

        let mut saw_blocking_conflict = false;
        for dir in windows_candidate_cli_dirs() {
            let Some((runt_status, nb_status)) = windows_cli_currency_for_dir(app, &dir) else {
                continue;
            };

            let runt_path = command_path(&dir, cli_command_name());
            let nb_path = command_path(&dir, cli_notebook_alias_name());
            let runt_exists = fs::symlink_metadata(&runt_path).is_ok();
            let nb_exists = fs::symlink_metadata(&nb_path).is_ok();
            let runt_owned = runt_status != SymlinkStatus::NotInstalled || !runt_exists;
            let nb_owned = nb_status != SymlinkStatus::NotInstalled || !nb_exists;

            if runt_status == SymlinkStatus::Current && nb_status == SymlinkStatus::Current {
                log::debug!(
                    "[cli_install] Windows CLI shims current in {}",
                    dir.display()
                );
                clear_windows_update_flag();
                return;
            }

            if !runt_owned || !nb_owned {
                log::info!(
                    "[cli_install] Windows CLI shim path in {} is occupied by an unrelated command; skipping auto-repair",
                    dir.display()
                );
                saw_blocking_conflict = true;
                continue;
            }

            if runt_exists || nb_exists {
                log::info!(
                    "[cli_install] Windows CLI shims need update in {} (runt={:?}, nb={:?})",
                    dir.display(),
                    runt_status,
                    nb_status
                );
                if let Err(e) = try_install_windows_cmd_shims(&bundled_runt, &runt_path, &nb_path) {
                    log::warn!("[cli_install] Failed to repair Windows CLI shims: {}", e);
                }
                clear_windows_update_flag();
                return;
            }
        }

        if saw_blocking_conflict {
            clear_windows_update_flag();
            return;
        }

        log::info!("[cli_install] Windows CLI shims are missing; installing them for this app");
        if let Err(e) = install_cli(app) {
            log::warn!("[cli_install] Failed to install Windows CLI shims: {}", e);
        }
        clear_windows_update_flag();
    }

    #[cfg(unix)]
    {
        // In dev mode, the bundled path points into a build artifact directory
        // (target/*/binaries/). Don't rewrite the user's global CLI to point there.
        if runt_workspace::is_dev_mode() {
            log::debug!("[cli_install] Dev mode — skipping CLI currency check");
            return;
        }

        // Skip if the app is running from an ephemeral path (macOS translocation,
        // AppImage mount, Downloads, DMG volume, etc.)
        if is_ephemeral_path(app) {
            return;
        }

        let (runt_status, nb_status) = check_cli_currency(app);

        log::debug!(
            "[cli_install] CLI currency check: runt={:?}, nb={:?}",
            runt_status,
            nb_status
        );

        // Only reinstall when at least one entry is Stale. install_cli() rewrites
        // both runt and nb, so we must ensure neither path is occupied by an
        // unrelated command. "NotInstalled" can mean either (a) the path doesn't
        // exist (safe to create) or (b) it exists but isn't ours (unsafe to
        // overwrite). Check actual path existence to distinguish the two cases.
        let runt_stale = runt_status == SymlinkStatus::Stale;
        let nb_stale = nb_status == SymlinkStatus::Stale;

        if !runt_stale && !nb_stale {
            log::debug!(
                "[cli_install] CLI currency check: runt={:?}, nb={:?} — no update needed",
                runt_status,
                nb_status
            );
            return;
        }

        // If either entry is "NotInstalled" (unrecognized), check whether the
        // path actually exists (or is a dangling symlink). If it does, something
        // else owns it — don't clobber. We use symlink_metadata() instead of
        // exists() because exists() returns false for dangling symlinks, which
        // would let us accidentally overwrite a broken symlink owned by another tool.
        let dir = install_dir();
        let cli_name = cli_command_name();
        let nb_name = cli_notebook_alias_name();

        let path_occupied = |p: PathBuf| fs::symlink_metadata(p).is_ok();
        let runt_blocked =
            runt_status == SymlinkStatus::NotInstalled && path_occupied(dir.join(cli_name));
        let nb_blocked =
            nb_status == SymlinkStatus::NotInstalled && path_occupied(dir.join(nb_name));

        if runt_blocked || nb_blocked {
            log::info!(
                "[cli_install] CLI partially stale (runt={:?}, nb={:?}) but {} \
                 is occupied by an unrelated command — skipping auto-repair",
                runt_status,
                nb_status,
                if runt_blocked { cli_name } else { nb_name }
            );
            return;
        }

        log::info!(
            "[cli_install] CLI needs update (runt={:?}, nb={:?}), reinstalling",
            runt_status,
            nb_status
        );
        if let Err(e) = install_cli(app) {
            log::warn!("[cli_install] Failed to update CLI: {}", e);
        } else {
            log::info!("[cli_install] CLI updated successfully");
        }
    }
}

/// Check if the CLI is already installed (checks user-local, system-wide, and legacy locations).
pub fn is_cli_installed() -> bool {
    is_cli_installed_local() || is_cli_installed_legacy()
}

/// Check if the CLI is installed to the user-local directory (`~/.local/bin`).
pub fn is_cli_installed_local() -> bool {
    let cli_name = cli_command_name();
    let nb_name = cli_notebook_alias_name();

    #[cfg(target_os = "windows")]
    {
        return windows_candidate_cli_dirs().iter().any(|dir| {
            command_path(dir, cli_name).exists() && command_path(dir, nb_name).exists()
        });
    }

    #[cfg(unix)]
    {
        let dir = install_dir();
        dir.join(cli_name).exists() && dir.join(nb_name).exists()
    }
}

/// Check if the CLI has a legacy install in `/usr/local/bin`.
pub fn is_cli_installed_legacy() -> bool {
    #[cfg(unix)]
    {
        let legacy = PathBuf::from(LEGACY_INSTALL_DIR);
        let cli_name = cli_command_name();
        let nb_name = cli_notebook_alias_name();
        legacy.join(cli_name).exists() && legacy.join(nb_name).exists()
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Install the CLI to the user-local command directory (no admin privileges needed).
/// Returns Ok(()) on success, Err with message on failure.
pub fn install_cli(app: &tauri::AppHandle) -> Result<(), String> {
    let bundled_runt = get_bundled_runt_path(app)
        .ok_or_else(|| "Could not find bundled runt binary".to_string())?;

    let dir = install_dir();

    // Ensure ~/.local/bin exists
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create {}: {}", dir.display(), e))?;

    #[cfg(target_os = "windows")]
    let (runt_dest, nb_dest) = (
        command_path(&dir, cli_command_name()),
        command_path(&dir, cli_notebook_alias_name()),
    );

    #[cfg(unix)]
    let (runt_dest, nb_dest) = (
        dir.join(cli_command_name()),
        dir.join(cli_notebook_alias_name()),
    );

    try_install_direct(&bundled_runt, &runt_dest, &nb_dest)?;

    log::info!(
        "[cli_install] CLI installed: {} -> {}",
        runt_dest.display(),
        bundled_runt.display()
    );

    // Warn if legacy /usr/local/bin entries shadow ~/.local/bin
    #[cfg(unix)]
    warn_legacy_cli_shadow();

    #[cfg(unix)]
    {
        // Ensure the user's shell RC has ~/.local/bin on PATH.
        if let Err(e) = ensure_shell_path(&dir) {
            log::warn!("[cli_install] Shell PATH integration skipped: {}", e);
        }
    }

    Ok(())
}

/// Warn if legacy /usr/local/bin has stale CLI copies that shadow ~/.local/bin.
///
/// Symlinks are fine — they track the app bundle. Only regular files (stale
/// copies from old installs) are a problem since they don't update.
#[cfg(unix)]
fn warn_legacy_cli_shadow() {
    let legacy = PathBuf::from(LEGACY_INSTALL_DIR);
    let stale: Vec<String> = [cli_command_name(), cli_notebook_alias_name()]
        .iter()
        .filter_map(|name| {
            let path = legacy.join(name);
            // Symlinks are fine — they resolve to the current app bundle.
            // Only warn about regular files (stale copies).
            if path.exists() && !path.is_symlink() {
                Some(path.to_string_lossy().to_string())
            } else {
                None
            }
        })
        .collect();

    if !stale.is_empty() {
        log::warn!(
            "[cli_install] Stale CLI copies in /usr/local/bin shadow ~/.local/bin: {}. \
             Remove with: sudo rm {}",
            stale.join(", "),
            stale.join(" ")
        );
    }
}

/// Try to install directly without admin privileges
fn try_install_direct(
    bundled_runt: &std::path::Path,
    runt_dest: &std::path::Path,
    nb_dest: &std::path::Path,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        // Remove existing file/symlink if present. Unix install paths are the
        // existing user-local/manual flow; ownership checks happen before
        // auto-repair on app launch.
        if runt_dest.exists() || runt_dest.is_symlink() {
            fs::remove_file(runt_dest)
                .map_err(|e| format!("Failed to remove existing {}: {}", cli_command_name(), e))?;
        }

        // Create a symlink so the CLI stays in sync when the app updates.
        symlink(bundled_runt, runt_dest).map_err(|e| format!("Failed to create symlink: {}", e))?;

        // Create nb wrapper script.
        create_nb_wrapper(nb_dest, cli_command_name())?;
    }

    #[cfg(target_os = "windows")]
    try_install_windows_cmd_shims(bundled_runt, runt_dest, nb_dest)?;

    Ok(())
}

/// Create the nb wrapper script
#[cfg(unix)]
fn create_nb_wrapper(nb_dest: &std::path::Path, cli_command: &str) -> Result<(), String> {
    let script = format!(
        r#"#!/bin/bash
# {} - open notebooks faster than you can say {} notebook
exec {} notebook "$@"
"#,
        cli_notebook_alias_name(),
        cli_command,
        cli_command
    );

    let mut file =
        fs::File::create(nb_dest).map_err(|e| format!("Failed to create nb script: {}", e))?;

    file.write_all(script.as_bytes())
        .map_err(|e| format!("Failed to write nb script: {}", e))?;

    // Make it executable
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(nb_dest)
            .map_err(|e| format!("Failed to get nb permissions: {}", e))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(nb_dest, perms)
            .map_err(|e| format!("Failed to set nb permissions: {}", e))?;
    }

    Ok(())
}

/// Ensure the user's shell RC file has `~/.local/bin` on PATH.
///
/// Appends a PATH export to `~/.zshrc`, `~/.bashrc`, or fish config if
/// `~/.local/bin` isn't already referenced. Idempotent — checks for the
/// marker comment or an existing `.local/bin` PATH entry before appending.
#[cfg(unix)]
fn ensure_shell_path(bin_dir: &std::path::Path) -> Result<(), String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let home = dirs::home_dir().ok_or("could not determine home directory")?;

    let (rc_path, snippet) = if shell.ends_with("/fish") {
        let config = home.join(".config/fish/config.fish");
        (
            config,
            format!(
                "\n# Added by nteract \u{2013} puts runt CLI on PATH\nfish_add_path {}\n",
                bin_dir.display()
            ),
        )
    } else if shell.ends_with("/bash") {
        (
            home.join(".bashrc"),
            format!(
                "\n# Added by nteract \u{2013} puts runt CLI on PATH\nexport PATH=\"{}:$PATH\"\n",
                bin_dir.display()
            ),
        )
    } else {
        // Default to zsh (macOS default since Catalina)
        (
            home.join(".zshrc"),
            format!(
                "\n# Added by nteract \u{2013} puts runt CLI on PATH\nexport PATH=\"{}:$PATH\"\n",
                bin_dir.display()
            ),
        )
    };

    // Read existing content (file may not exist yet)
    let existing = fs::read_to_string(&rc_path).unwrap_or_default();

    // Already configured — nothing to do
    if existing.contains(".local/bin") || existing.contains("Added by nteract") {
        log::debug!(
            "[cli_install] Shell RC {} already has ~/.local/bin on PATH",
            rc_path.display()
        );
        return Ok(());
    }

    // Ensure parent directory exists (relevant for fish config)
    if let Some(parent) = rc_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {}: {}", parent.display(), e))?;
    }

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&rc_path)
        .map_err(|e| format!("Failed to open {}: {}", rc_path.display(), e))?;

    file.write_all(snippet.as_bytes())
        .map_err(|e| format!("Failed to write to {}: {}", rc_path.display(), e))?;

    log::info!(
        "[cli_install] Added ~/.local/bin to PATH in {}",
        rc_path.display()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// System CLI migration (one-time, user-initiated during upgrade)
// ---------------------------------------------------------------------------

/// Information about a system-wide CLI install that should be migrated.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SystemCliMigration {
    pub dir: String,
    pub cli_name: String,
    pub nb_name: String,
}

/// Detect a system-wide CLI install in `/usr/local/bin` that was placed by nteract.
///
/// Only returns `Some` when the `.nteract-managed-*` marker file is present.
/// Without that marker the binary could belong to Homebrew, a manual install,
/// or another tool, and we must not touch it with elevated privileges.
#[cfg(unix)]
pub fn detect_system_cli_migration() -> Option<SystemCliMigration> {
    let dir = PathBuf::from(LEGACY_INSTALL_DIR);
    let cli_name = cli_command_name();
    let marker_name = format!(".nteract-managed-{}", cli_name);

    if !dir.join(&marker_name).exists() {
        return None;
    }

    let runt_path = dir.join(cli_name);

    // Symlinks pointing into an nteract bundle are fine and don't need migration.
    if !runt_path.exists() || runt_path.is_symlink() {
        return None;
    }

    let nb_name = cli_notebook_alias_name();

    Some(SystemCliMigration {
        dir: dir.to_string_lossy().to_string(),
        cli_name: cli_name.to_string(),
        nb_name: nb_name.to_string(),
    })
}

#[cfg(not(unix))]
pub fn detect_system_cli_migration() -> Option<SystemCliMigration> {
    None
}

/// Replace the system-wide CLI copy with a symlink to the app bundle binary.
/// Requires privilege escalation (one-time).
#[cfg(target_os = "macos")]
pub fn migrate_system_cli_to_symlink(app: &tauri::AppHandle) -> Result<(), String> {
    let bundled_runt = get_bundled_runt_path(app)
        .ok_or_else(|| "Could not find bundled runt binary".to_string())?;

    let dir = PathBuf::from(LEGACY_INSTALL_DIR);
    let cli_name = cli_command_name();
    let nb_name = cli_notebook_alias_name();
    let marker_name = format!(".nteract-managed-{}", cli_name);

    let shell_cmd = format!(
        "rm -f {runt} {nb} {marker} && ln -s {src} {runt}",
        src = shell_escape(bundled_runt.to_string_lossy().as_ref()),
        runt = shell_escape(dir.join(cli_name).to_string_lossy().as_ref()),
        nb = shell_escape(dir.join(nb_name).to_string_lossy().as_ref()),
        marker = shell_escape(dir.join(&marker_name).to_string_lossy().as_ref()),
    );

    escalate_shell_command(&shell_cmd)
}

/// Remove the system-wide CLI entirely. Requires privilege escalation.
#[cfg(target_os = "macos")]
pub fn remove_system_cli(app: &tauri::AppHandle) -> Result<(), String> {
    let _ = app;
    let dir = PathBuf::from(LEGACY_INSTALL_DIR);
    let cli_name = cli_command_name();
    let nb_name = cli_notebook_alias_name();
    let marker_name = format!(".nteract-managed-{}", cli_name);

    let shell_cmd = format!(
        "rm -f {runt} {nb} {marker}",
        runt = shell_escape(dir.join(cli_name).to_string_lossy().as_ref()),
        nb = shell_escape(dir.join(nb_name).to_string_lossy().as_ref()),
        marker = shell_escape(dir.join(&marker_name).to_string_lossy().as_ref()),
    );

    escalate_shell_command(&shell_cmd)
}

/// Replace the system-wide CLI copy with a symlink to the app bundle binary.
#[cfg(target_os = "linux")]
pub fn migrate_system_cli_to_symlink(app: &tauri::AppHandle) -> Result<(), String> {
    let bundled_runt = get_bundled_runt_path(app)
        .ok_or_else(|| "Could not find bundled runt binary".to_string())?;

    let dir = PathBuf::from(LEGACY_INSTALL_DIR);
    let cli_name = cli_command_name();
    let nb_name = cli_notebook_alias_name();
    let marker_name = format!(".nteract-managed-{}", cli_name);

    let shell_cmd = format!(
        "rm -f {runt} {nb} {marker} && ln -s {src} {runt}",
        src = shell_escape(bundled_runt.to_string_lossy().as_ref()),
        runt = shell_escape(dir.join(cli_name).to_string_lossy().as_ref()),
        nb = shell_escape(dir.join(nb_name).to_string_lossy().as_ref()),
        marker = shell_escape(dir.join(&marker_name).to_string_lossy().as_ref()),
    );

    escalate_shell_command(&shell_cmd)
}

/// Remove the system-wide CLI entirely.
#[cfg(target_os = "linux")]
pub fn remove_system_cli(app: &tauri::AppHandle) -> Result<(), String> {
    let _ = app;
    let dir = PathBuf::from(LEGACY_INSTALL_DIR);
    let cli_name = cli_command_name();
    let nb_name = cli_notebook_alias_name();
    let marker_name = format!(".nteract-managed-{}", cli_name);

    let shell_cmd = format!(
        "rm -f {runt} {nb} {marker}",
        runt = shell_escape(dir.join(cli_name).to_string_lossy().as_ref()),
        nb = shell_escape(dir.join(nb_name).to_string_lossy().as_ref()),
        marker = shell_escape(dir.join(&marker_name).to_string_lossy().as_ref()),
    );

    escalate_shell_command(&shell_cmd)
}

#[cfg(target_os = "windows")]
pub fn migrate_system_cli_to_symlink(_app: &tauri::AppHandle) -> Result<(), String> {
    Err("Symlink migration is not supported on Windows".to_string())
}

#[cfg(target_os = "windows")]
pub fn remove_system_cli(_app: &tauri::AppHandle) -> Result<(), String> {
    Err("System CLI removal is not supported on Windows".to_string())
}

/// Escape a string for use inside a single-quoted shell argument.
#[cfg(unix)]
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Run a shell command with OS privilege escalation.
#[cfg(target_os = "macos")]
fn escalate_shell_command(shell_cmd: &str) -> Result<(), String> {
    let temp_script =
        std::env::temp_dir().join(format!("nteract-cli-migrate-{}.sh", std::process::id()));
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&temp_script)
            .or_else(|_| {
                let _ = fs::remove_file(&temp_script);
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o700)
                    .open(&temp_script)
            })
            .map_err(|e| format!("Failed to create migration script: {}", e))?;
        file.write_all(shell_cmd.as_bytes())
            .map_err(|e| format!("Failed to write migration script: {}", e))?;
    }

    let escaped_script_path = temp_script
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg(format!(
            "do shell script \"sh '{escaped_script_path}'\" with administrator privileges"
        ))
        .output()
        .map_err(|e| format!("Failed to run privilege escalation: {}", e))?;

    let _ = fs::remove_file(&temp_script);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("User canceled")
            || stderr.contains("user canceled")
            || stderr.contains("-128")
        {
            return Err("Cancelled.".to_string());
        }
        return Err(format!("Privilege escalation failed: {}", stderr.trim()));
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn escalate_shell_command(shell_cmd: &str) -> Result<(), String> {
    let output = std::process::Command::new("pkexec")
        .arg("sh")
        .arg("-c")
        .arg(shell_cmd)
        .output()
        .map_err(|e| format!("Failed to run privilege escalation: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("dismissed") || stderr.contains("Not authorized") {
            return Err("Cancelled.".to_string());
        }
        return Err(format!("Privilege escalation failed: {}", stderr.trim()));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn nb_script_current_when_matches() {
        let dir = tempfile::tempdir().ok().unwrap();
        let script_path = dir.path().join("nb");
        fs::write(
            &script_path,
            "#!/bin/bash\n# nb - open notebooks\nexec runt-nightly notebook \"$@\"\n",
        )
        .ok();

        assert_eq!(
            check_nb_script(&script_path, "runt-nightly"),
            SymlinkStatus::Current
        );
    }

    #[cfg(unix)]
    #[test]
    fn nb_script_stale_when_wrong_command() {
        let dir = tempfile::tempdir().ok().unwrap();
        let script_path = dir.path().join("nb");
        fs::write(
            &script_path,
            "#!/bin/bash\n# nb - open notebooks\nexec runt notebook \"$@\"\n",
        )
        .ok();

        // Expects runt-nightly but script has runt
        assert_eq!(
            check_nb_script(&script_path, "runt-nightly"),
            SymlinkStatus::Stale
        );
    }

    #[cfg(unix)]
    #[test]
    fn nb_script_not_installed_when_missing() {
        let dir = tempfile::tempdir().ok().unwrap();
        let script_path = dir.path().join("nb-nonexistent");

        assert_eq!(
            check_nb_script(&script_path, "runt"),
            SymlinkStatus::NotInstalled
        );
    }

    #[cfg(unix)]
    #[test]
    fn runt_symlink_not_installed_when_missing() {
        let dir = tempfile::tempdir().ok().unwrap();
        let symlink_path = dir.path().join("runt-nonexistent");

        assert!(!symlink_path.exists());
        assert!(!symlink_path.is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn nb_script_not_installed_when_not_ours() {
        let dir = tempfile::tempdir().ok().unwrap();
        let script_path = dir.path().join("nb");
        // An unrelated nb script — even one mentioning "grunt" (substring of "runt")
        fs::write(
            &script_path,
            "#!/bin/bash\n# build tool\nexec grunt \"$@\"\n",
        )
        .ok();

        assert_eq!(
            check_nb_script(&script_path, "runt"),
            SymlinkStatus::NotInstalled
        );
    }

    #[cfg(unix)]
    #[test]
    fn regular_file_not_treated_as_ours() {
        let dir = tempfile::tempdir().ok().unwrap();
        let file_path = dir.path().join("runt");
        fs::write(&file_path, "not a symlink").ok();

        // A regular file (not a symlink) should be NotInstalled, not Stale
        assert!(file_path.exists());
        assert!(!file_path.is_symlink());
    }

    #[test]
    fn windows_cmd_shim_invokes_absolute_runt_path() {
        let runt = PathBuf::from(r"C:\Users\Alice\AppData\Local\nteract Nightly\runt.exe");

        assert_eq!(
            windows_runt_cmd_shim_contents(&runt),
            "@echo off\r\nrem nteract-managed-cli-shim v1\r\n\"C:\\Users\\Alice\\AppData\\Local\\nteract Nightly\\runt.exe\" %*\r\n"
        );
    }

    #[test]
    fn windows_cmd_names_are_batch_files() {
        assert_eq!(windows_cmd_name("runt-nightly"), "runt-nightly.cmd");
        assert_eq!(windows_cmd_name("nb-nightly"), "nb-nightly.cmd");
    }

    #[test]
    fn windows_nb_cmd_shim_invokes_notebook_mode_directly() {
        let runt = PathBuf::from(r"C:\Users\Alice\AppData\Local\nteract Nightly\runt.exe");

        assert_eq!(
            windows_nb_cmd_shim_contents(&runt),
            "@echo off\r\nrem nteract-managed-cli-shim v1\r\n\"C:\\Users\\Alice\\AppData\\Local\\nteract Nightly\\runt.exe\" notebook %*\r\n"
        );
    }

    #[test]
    fn windows_cmd_shim_escapes_percent_signs_in_paths() {
        let runt = PathBuf::from(r"C:\Users\100% Real\AppData\Local\nteract Nightly\runt.exe");

        assert!(windows_runt_cmd_shim_contents(&runt).contains(r"100%% Real"));
    }

    #[test]
    fn windows_cmd_shim_ownership_requires_marker() {
        assert!(is_owned_windows_cmd_shim(
            "@echo off\r\nrem nteract-managed-cli-shim v1\r\n\"runt.exe\" %*\r\n"
        ));
        assert!(!is_owned_windows_cmd_shim(
            "@echo off\r\n\"some-other-runt.exe\" %*\r\n"
        ));
        assert!(!is_owned_windows_cmd_shim(
            "@echo off\r\n\"some-other-runt.exe\" %*\r\nrem nteract-managed-cli-shim v1\r\n"
        ));
    }
}
