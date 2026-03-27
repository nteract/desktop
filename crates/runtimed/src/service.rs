//! Cross-platform service management for runtimed.
//!
//! Handles installation and management of the daemon as a system service:
//! - macOS: launchd user agent (channel-specific `io.nteract.runtimed*.plist`)
//! - Linux: systemd user service (channel-specific `runtimed*.service`)
//! - Windows: Startup shortcut

use std::path::{Path, PathBuf};

use log::info;
use runt_workspace::cache_namespace;
#[cfg(any(target_os = "linux", target_os = "windows"))]
use runt_workspace::daemon_binary_basename;
#[cfg(target_os = "macos")]
use runt_workspace::daemon_launchd_label;
#[cfg(any(target_os = "linux", target_os = "windows"))]
use runt_workspace::daemon_service_basename;

/// Service configuration.
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    /// Path to the daemon binary.
    pub binary_path: PathBuf,
    /// Path to the log file.
    pub log_path: PathBuf,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            binary_path: default_binary_path(),
            log_path: default_log_path(),
        }
    }
}

/// Get the default destination path for the daemon binary.
///
/// On macOS, when the current process is running inside an app bundle (i.e.
/// as a Tauri sidecar), returns the sidecar's own path — the plist will point
/// directly at it and no copy is needed.
///
/// For all other callers (CLI, standalone `runtimed install`, etc.), returns
/// the standalone install location (`~/.local/share/runt/bin/runtimed`).
/// The `install()` and `upgrade()` methods handle the in-bundle → skip-copy
/// logic based on the *source* binary, not this default.
pub fn default_binary_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        // When running as a Tauri sidecar, current_exe IS the in-bundle binary.
        // Use it directly so the plist points at the app bundle.
        if let Ok(exe) = std::env::current_exe() {
            if exe.to_string_lossy().contains(".app/Contents/MacOS/") {
                return exe;
            }
        }
        // For CLI / standalone callers, use the traditional install location.
        // install() and upgrade() will override this if source_binary is in-bundle.
        runt_workspace::legacy_standalone_binary_path()
    }

    #[cfg(target_os = "linux")]
    {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(cache_namespace())
            .join("bin")
            .join(daemon_binary_basename())
    }

    #[cfg(target_os = "windows")]
    {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("C:\\temp"))
            .join(cache_namespace())
            .join("bin")
            .join(format!("{}.exe", daemon_binary_basename()))
    }
}

/// Get the default path for the daemon log file.
///
/// Note: This is the system service log path (always ~/.cache/runt/runtimed.log).
/// For dev mode, use `crate::default_log_path()` which handles per-worktree paths.
pub fn default_log_path() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(cache_namespace())
        .join("runtimed.log")
}

/// Result type for service operations.
pub type ServiceResult<T> = Result<T, ServiceError>;

/// Errors that can occur during service operations.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Service already installed")]
    AlreadyInstalled,

    #[error("Service not installed")]
    NotInstalled,

    #[error("Binary not found at {0}")]
    BinaryNotFound(PathBuf),

    #[error("Failed to start service: {0}")]
    StartFailed(String),

    #[error("Failed to stop service: {0}")]
    StopFailed(String),

    #[error("Failed to install service: {0}")]
    InstallFailed(String),

    #[error("Unsupported platform")]
    UnsupportedPlatform,
}

/// Service manager for runtimed.
pub struct ServiceManager {
    config: ServiceConfig,
}

impl Default for ServiceManager {
    fn default() -> Self {
        Self::new(ServiceConfig::default())
    }
}

impl ServiceManager {
    /// Create a new service manager with the given configuration.
    pub fn new(config: ServiceConfig) -> Self {
        Self { config }
    }

    /// Install the daemon as a system service.
    ///
    /// On macOS, if the source binary is inside an app bundle, the plist is
    /// pointed directly at it — no copy is needed. This avoids the macOS
    /// "App Management" permission prompt and code-signature issues during
    /// upgrades. If the source is a custom path (e.g. `--binary /path/to/runtimed`),
    /// it is honored and copied to the configured install location as before.
    pub fn install(&mut self, source_binary: &PathBuf) -> ServiceResult<()> {
        if !source_binary.exists() {
            return Err(ServiceError::BinaryNotFound(source_binary.clone()));
        }

        if Self::is_in_app_bundle(source_binary) {
            // Source is inside an app bundle — point the plist directly at it.
            self.config.binary_path = source_binary.clone();
            info!(
                "[service] Using in-bundle binary at {:?}",
                self.config.binary_path
            );
            Self::cleanup_legacy_binary();
        } else {
            // Custom or non-bundle path — copy the binary to the install location
            if let Some(parent) = self.config.binary_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            self.atomic_copy_binary(source_binary)?;
        }

        // Create service configuration
        self.create_service_config()?;

        info!("[service] Service installed successfully");
        Ok(())
    }

    /// Copy a binary to `self.config.binary_path` via a temporary file and
    /// atomic `rename`, then set permissions and remove quarantine.
    ///
    /// A plain `std::fs::copy` truncates and rewrites the *same inode*.
    /// On macOS, if a `KeepAlive`-restarted daemon still has the old inode
    /// memory-mapped, the in-place write invalidates its code-signature
    /// pages.  Worse, the *new* daemon inherits the same inode and can
    /// crash minutes later when macOS demand-pages an unloaded `__TEXT`
    /// page whose hash no longer matches the code directory.
    ///
    /// Writing to a temp file and then `rename`-ing atomically swaps the
    /// directory entry to a **new inode**, so:
    ///   - any process still mapped to the old inode keeps valid pages,
    ///   - the new daemon maps a pristine inode with a clean signature.
    fn atomic_copy_binary(&self, source_binary: &PathBuf) -> ServiceResult<()> {
        let tmp_path = self.config.binary_path.with_extension("new");

        // Copy to a temp file (creates a new inode)
        std::fs::copy(source_binary, &tmp_path)?;

        // Set permissions on the temp file before rename
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(&tmp_path, perms)?;
        }

        // Remove quarantine on the temp file before rename
        #[cfg(target_os = "macos")]
        {
            use std::process::Command;
            // Best-effort: if quarantine removal fails, the rename still
            // proceeds — Gatekeeper may prompt but won't crash.
            let _ = Command::new("xattr")
                .args(["-d", "com.apple.quarantine"])
                .arg(&tmp_path)
                .output();
        }

        // Atomic swap — old inode stays valid for any mapped process
        std::fs::rename(&tmp_path, &self.config.binary_path)?;

        info!(
            "[service] Installed binary to {:?}",
            self.config.binary_path
        );

        Ok(())
    }

    /// Uninstall the daemon service.
    pub fn uninstall(&self) -> ServiceResult<()> {
        // Stop the service first
        self.stop().ok();

        // Remove service configuration
        self.remove_service_config()?;

        // Determine what binary the plist was pointing at (if readable)
        #[cfg(target_os = "macos")]
        let plist_bin = runt_workspace::plist_binary_path();
        #[cfg(target_os = "macos")]
        let binary_was_in_bundle = plist_bin
            .as_ref()
            .map(|p| Self::is_in_app_bundle(p))
            .unwrap_or_else(|| Self::is_in_app_bundle(&self.config.binary_path));

        #[cfg(not(target_os = "macos"))]
        let binary_was_in_bundle = false;

        if binary_was_in_bundle {
            // Don't delete the in-bundle binary — it belongs to the app.
            // Only clean up the legacy standalone binary if it still exists.
            Self::cleanup_legacy_binary();
        } else {
            // Remove standalone binary
            if self.config.binary_path.exists() {
                std::fs::remove_file(&self.config.binary_path)?;
                info!("[service] Removed binary {:?}", self.config.binary_path);
            }
            if let Some(parent) = self.config.binary_path.parent() {
                std::fs::remove_dir(parent).ok();
            }
        }

        info!("[service] Service uninstalled successfully");
        Ok(())
    }

    /// Upgrade the daemon binary by stopping, replacing, and restarting.
    ///
    /// This is used when the notebook app detects a version mismatch between
    /// the running daemon and the bundled version. On macOS with in-bundle
    /// binaries, the binary is already updated (the app was replaced), so this
    /// just regenerates the plist and restarts launchd.
    pub fn upgrade(&mut self, source_binary: &PathBuf) -> ServiceResult<()> {
        if !source_binary.exists() {
            return Err(ServiceError::BinaryNotFound(source_binary.clone()));
        }

        info!("[service] Upgrading daemon binary from {:?}", source_binary);

        // Stop the running daemon (ignore errors - may not be running)
        self.stop().ok();

        if Self::is_in_app_bundle(source_binary) {
            // Source is inside an app bundle — point the plist directly at it.
            self.config.binary_path = source_binary.clone();
            info!("[service] In-bundle binary, skipping copy");
            Self::cleanup_legacy_binary();
        } else {
            // Standalone binary (Linux, Windows, edge cases) — atomically replace.
            self.atomic_copy_binary(source_binary)?;
        }

        // Recreate service config to apply any template changes (e.g., new env vars,
        // or to migrate the plist from a standalone path to an in-bundle path)
        self.create_service_config()?;
        info!("[service] Updated service config");

        // Use launchd_start() which always does bootout+bootstrap, not
        // start() which uses ensure_loaded (a no-op if KeepAlive already
        // restarted the old binary during the stop→copy window).
        #[cfg(target_os = "macos")]
        runt_workspace::launchd_start().map_err(ServiceError::StartFailed)?;

        #[cfg(not(target_os = "macos"))]
        self.start()?;

        info!("[service] Upgrade completed successfully");
        Ok(())
    }

    /// Check whether a path is inside a macOS `.app` bundle.
    fn is_in_app_bundle(path: &Path) -> bool {
        path.to_string_lossy().contains(".app/Contents/MacOS/")
    }

    /// Remove the legacy standalone binary from `~/.local/share/runt/bin/`.
    ///
    /// Best-effort: logs on failure but never errors. This cleans up the
    /// pre-migration install where the binary was copied out of the app bundle.
    fn cleanup_legacy_binary() {
        #[cfg(target_os = "macos")]
        {
            let legacy = runt_workspace::legacy_standalone_binary_path();
            if legacy.exists() {
                match std::fs::remove_file(&legacy) {
                    Ok(()) => info!("[service] Removed legacy standalone binary at {:?}", legacy),
                    Err(e) => info!(
                        "[service] Could not remove legacy binary {:?}: {}",
                        legacy, e
                    ),
                }
                // Try to remove parent dirs if empty
                if let Some(parent) = legacy.parent() {
                    std::fs::remove_dir(parent).ok();
                    if let Some(grandparent) = parent.parent() {
                        std::fs::remove_dir(grandparent).ok();
                    }
                }
            }
        }
    }

    /// Start the daemon service.
    pub fn start(&self) -> ServiceResult<()> {
        #[cfg(target_os = "macos")]
        {
            self.start_macos()
        }

        #[cfg(target_os = "linux")]
        {
            self.start_linux()
        }

        #[cfg(target_os = "windows")]
        {
            self.start_windows()
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            Err(ServiceError::UnsupportedPlatform)
        }
    }

    /// Stop the daemon service.
    pub fn stop(&self) -> ServiceResult<()> {
        #[cfg(target_os = "macos")]
        {
            self.stop_macos()
        }

        #[cfg(target_os = "linux")]
        {
            self.stop_linux()
        }

        #[cfg(target_os = "windows")]
        {
            self.stop_windows()
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            Err(ServiceError::UnsupportedPlatform)
        }
    }

    /// Check if the service is installed.
    pub fn is_installed(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            plist_path().exists()
        }

        #[cfg(target_os = "linux")]
        {
            systemd_service_path().exists()
        }

        #[cfg(target_os = "windows")]
        {
            windows_startup_path().exists()
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            false
        }
    }

    /// Create the platform-specific service configuration.
    fn create_service_config(&self) -> ServiceResult<()> {
        #[cfg(target_os = "macos")]
        {
            self.create_macos_plist()
        }

        #[cfg(target_os = "linux")]
        {
            self.create_linux_systemd()
        }

        #[cfg(target_os = "windows")]
        {
            self.create_windows_startup()
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            Err(ServiceError::UnsupportedPlatform)
        }
    }

    /// Remove the platform-specific service configuration.
    fn remove_service_config(&self) -> ServiceResult<()> {
        #[cfg(target_os = "macos")]
        {
            let path = plist_path();
            if path.exists() {
                std::fs::remove_file(&path)?;
                info!("[service] Removed {:?}", path);
            }
            Ok(())
        }

        #[cfg(target_os = "linux")]
        {
            let path = systemd_service_path();
            if path.exists() {
                std::fs::remove_file(&path)?;
                info!("[service] Removed {:?}", path);
                // Reload systemd
                std::process::Command::new("systemctl")
                    .args(["--user", "daemon-reload"])
                    .output()
                    .ok();
            }
            Ok(())
        }

        #[cfg(target_os = "windows")]
        {
            let path = windows_startup_path();
            if path.exists() {
                std::fs::remove_file(&path)?;
                info!("[service] Removed {:?}", path);
            }
            Ok(())
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            Err(ServiceError::UnsupportedPlatform)
        }
    }

    // macOS-specific implementations
    #[cfg(target_os = "macos")]
    fn create_macos_plist(&self) -> ServiceResult<()> {
        // Get home directory at plist generation time - launchd doesn't expand ~
        let home = dirs::home_dir().ok_or_else(|| {
            ServiceError::InstallFailed(
                "Cannot determine home directory for service install".into(),
            )
        })?;
        let home_str = home.to_string_lossy();
        let user = std::env::var("USER").unwrap_or_else(|_| "unknown".into());

        let plist_content = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{binary}</string>
        <string>--log-level</string>
        <string>{log_level}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>Crashed</key>
        <true/>
    </dict>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>HOME</key>
        <string>{home}</string>
        <key>USER</key>
        <string>{user}</string>
        <key>PATH</key>
        <string>{home}/.local/bin:/usr/local/bin:/usr/bin:/bin:/opt/homebrew/bin</string>
    </dict>
</dict>
</plist>
"#,
            label = daemon_launchd_label(),
            binary = self.config.binary_path.display(),
            log_level = match runt_workspace::build_channel() {
                runt_workspace::BuildChannel::Nightly =>
                    "info,notebook_sync=debug,runtimed::notebook_sync_server=debug",
                runt_workspace::BuildChannel::Stable => "warn",
            },
            log = self.config.log_path.display(),
            home = home_str,
            user = user,
        );

        let plist_path = plist_path();
        if let Some(parent) = plist_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::write(&plist_path, plist_content)?;
        info!("[service] Created {:?}", plist_path);

        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn start_macos(&self) -> ServiceResult<()> {
        let bootstrapped =
            runt_workspace::launchd_ensure_loaded().map_err(ServiceError::StartFailed)?;

        if bootstrapped {
            info!("[service] Started launchd service");
        } else {
            info!("[service] Launchd service already loaded");
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn stop_macos(&self) -> ServiceResult<()> {
        runt_workspace::launchd_stop().map_err(ServiceError::StopFailed)?;

        info!("[service] Stopped launchd service");
        Ok(())
    }

    // Linux-specific implementations
    #[cfg(target_os = "linux")]
    fn create_linux_systemd(&self) -> ServiceResult<()> {
        // Get home directory at service generation time - systemd doesn't expand ~
        let home = dirs::home_dir().ok_or_else(|| {
            ServiceError::InstallFailed(
                "Cannot determine home directory for service install".into(),
            )
        })?;
        let home_str = home.to_string_lossy();

        let service_name = daemon_service_basename();
        let service_content = format!(
            r#"[Unit]
Description={name} - Jupyter Runtime Daemon
After=network.target

[Service]
Type=simple
ExecStart={binary}
Restart=on-failure
RestartSec=5
Environment=HOME={home}
Environment=PATH={home}/.local/bin:/usr/local/bin:/usr/bin:/bin

[Install]
WantedBy=default.target
"#,
            name = service_name,
            binary = self.config.binary_path.display(),
            home = home_str,
        );

        let service_file_path = systemd_service_path();
        if let Some(parent) = service_file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::write(&service_file_path, service_content)?;
        info!("[service] Created {:?}", service_file_path);

        // Reload systemd
        std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .output()?;

        // Enable the service
        std::process::Command::new("systemctl")
            .args(["--user", "enable"])
            .arg(systemd_service_unit_name())
            .output()?;

        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn start_linux(&self) -> ServiceResult<()> {
        let output = std::process::Command::new("systemctl")
            .args(["--user", "start"])
            .arg(systemd_service_unit_name())
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ServiceError::StartFailed(stderr.to_string()));
        }

        info!("[service] Started systemd service");
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn stop_linux(&self) -> ServiceResult<()> {
        let output = std::process::Command::new("systemctl")
            .args(["--user", "stop"])
            .arg(systemd_service_unit_name())
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Ignore "not loaded" errors
            if !stderr.contains("not loaded") {
                return Err(ServiceError::StopFailed(stderr.to_string()));
            }
        }

        info!("[service] Stopped systemd service");
        Ok(())
    }

    // Windows-specific implementations
    #[cfg(target_os = "windows")]
    fn create_windows_startup(&self) -> ServiceResult<()> {
        // For Windows, we create a simple batch file in the Startup folder
        // A more robust solution would use the Task Scheduler API
        let startup_path = windows_startup_path();
        if let Some(parent) = startup_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Create a VBS script to start the daemon hidden
        let vbs_content = format!(
            r#"Set WshShell = CreateObject("WScript.Shell")
WshShell.Run chr(34) & "{}" & chr(34), 0
Set WshShell = Nothing
"#,
            self.config.binary_path.display(),
        );

        std::fs::write(&startup_path, vbs_content)?;
        info!("[service] Created {:?}", startup_path);

        Ok(())
    }

    #[cfg(target_os = "windows")]
    fn start_windows(&self) -> ServiceResult<()> {
        // Start the daemon directly
        std::process::Command::new(&self.config.binary_path)
            .spawn()
            .map_err(|e| ServiceError::StartFailed(e.to_string()))?;

        info!("[service] Started daemon process");
        Ok(())
    }

    #[cfg(target_os = "windows")]
    fn stop_windows(&self) -> ServiceResult<()> {
        let image_name = self
            .config
            .binary_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("runtimed.exe");

        // Kill the daemon process by name
        std::process::Command::new("taskkill")
            .args(["/F", "/IM", image_name])
            .output()
            .map_err(|e| ServiceError::StopFailed(e.to_string()))?;

        info!("[service] Stopped daemon process");
        Ok(())
    }
}

// Platform-specific paths

#[cfg(target_os = "macos")]
fn plist_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", daemon_launchd_label()))
}

#[cfg(target_os = "linux")]
fn systemd_service_unit_name() -> String {
    format!("{}.service", daemon_service_basename())
}

#[cfg(target_os = "linux")]
fn systemd_service_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("systemd")
        .join("user")
        .join(systemd_service_unit_name())
}

#[cfg(target_os = "windows")]
fn windows_startup_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("C:\\temp"))
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("Startup")
        .join(format!("{}.vbs", daemon_service_basename()))
}

/// Get the path to the service configuration file.
/// Used by doctor command for diagnostics.
pub fn service_config_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        plist_path()
    }
    #[cfg(target_os = "linux")]
    {
        systemd_service_path()
    }
    #[cfg(target_os = "windows")]
    {
        windows_startup_path()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        PathBuf::from("/dev/null")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_default_paths() {
        let binary = default_binary_path();
        let log = default_log_path();

        let binary_str = binary.to_string_lossy();
        // On macOS the path may be inside an app bundle or the legacy standalone location
        assert!(
            binary_str.contains("runtimed"),
            "binary path should contain 'runtimed': {binary_str}"
        );
        assert!(log.to_string_lossy().contains("runtimed.log"));
    }

    #[test]
    fn test_service_manager_default() {
        let manager = ServiceManager::default();
        // Just verify it doesn't panic
        let _ = manager.is_installed();
    }

    /// Verify the macOS plist template includes HOME env var (prevents startup failures)
    #[test]
    #[cfg(target_os = "macos")]
    fn test_plist_template_contains_home_env() {
        // Verify that dirs::home_dir() returns Some (prerequisite for the template)
        assert!(
            dirs::home_dir().is_some(),
            "HOME must be available for plist generation"
        );

        // Check the actual plist file if it exists (from a previous install)
        let plist_path = plist_path();
        if plist_path.exists() {
            let content = std::fs::read_to_string(&plist_path).unwrap();
            assert!(
                content.contains("<key>HOME</key>"),
                "Installed plist should contain HOME env var. \
                 If this fails, run 'runt daemon doctor --fix' to update the plist."
            );
        }
    }

    /// Verify the Linux systemd template includes HOME env var
    #[test]
    #[cfg(target_os = "linux")]
    fn test_systemd_template_contains_home_env() {
        // Verify that dirs::home_dir() returns Some (prerequisite for the template)
        assert!(
            dirs::home_dir().is_some(),
            "HOME must be available for systemd service generation"
        );

        // Check the actual service file if it exists
        let service_path = systemd_service_path();
        if service_path.exists() {
            let content = std::fs::read_to_string(&service_path).unwrap();
            assert!(
                content.contains("Environment=HOME="),
                "Installed systemd service should contain HOME env var"
            );
        }
    }
}
