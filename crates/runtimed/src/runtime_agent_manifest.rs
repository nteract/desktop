//! Per-runtime-agent ownership manifests for crash recovery.
//!
//! Jupyter connection files remain kernel transport configuration. These
//! manifests are daemon-owned process ownership records: one runtime agent, one
//! file, one cleanup decision.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

use crate::daemon_base_dir;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ManifestPlatform {
    Unix,
    Windows,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeAgentManifest {
    pub schema_version: u32,
    pub runtime_agent_id: String,
    pub notebook_id: String,
    pub daemon_pid: u32,
    pub created_at_unix_ms: u64,
    pub connection_file: Option<PathBuf>,
    pub platform: ManifestPlatform,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pgid: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_name: Option<String>,
}

impl RuntimeAgentManifest {
    #[cfg(unix)]
    pub fn unix(runtime_agent_id: String, notebook_id: String, pgid: i32) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            runtime_agent_id,
            notebook_id,
            daemon_pid: std::process::id(),
            created_at_unix_ms: unix_now_ms(),
            connection_file: None,
            platform: ManifestPlatform::Unix,
            pgid: Some(pgid),
            pid: None,
            job_name: None,
        }
    }

    #[cfg(windows)]
    pub fn windows(
        runtime_agent_id: String,
        notebook_id: String,
        pid: u32,
        job_name: String,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            runtime_agent_id,
            notebook_id,
            daemon_pid: std::process::id(),
            created_at_unix_ms: unix_now_ms(),
            connection_file: None,
            platform: ManifestPlatform::Windows,
            pgid: None,
            pid: Some(pid),
            job_name: Some(job_name),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupDecision {
    Reaped,
    Missing,
    RetainForRetry,
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn manifest_dir() -> PathBuf {
    manifest_dir_for_base(&daemon_base_dir())
}

fn legacy_agents_path() -> PathBuf {
    daemon_base_dir().join("agents.json")
}

#[cfg(unix)]
fn legacy_kernels_path() -> PathBuf {
    daemon_base_dir().join("kernels.json")
}

pub(crate) fn manifest_dir_for_base(base: &Path) -> PathBuf {
    base.join("runtime-agents")
}

pub(crate) fn manifest_file_name(runtime_agent_id: &str) -> String {
    let mut name = String::with_capacity(runtime_agent_id.len() + 5);
    for ch in runtime_agent_id.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            name.push(ch);
        } else {
            name.push('_');
        }
    }
    if name.is_empty() {
        name.push_str("runtime-agent");
    }
    name.push_str(".json");
    name
}

pub(crate) fn manifest_path_for_dir(dir: &Path, runtime_agent_id: &str) -> PathBuf {
    dir.join(manifest_file_name(runtime_agent_id))
}

pub fn manifest_path(runtime_agent_id: &str) -> PathBuf {
    manifest_path_for_dir(&manifest_dir(), runtime_agent_id)
}

pub fn write_manifest(manifest: &RuntimeAgentManifest) -> Result<()> {
    write_manifest_to_dir(&manifest_dir(), manifest)
}

pub(crate) fn write_manifest_to_dir(dir: &Path, manifest: &RuntimeAgentManifest) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| {
        format!(
            "failed to create runtime-agent manifest dir {}",
            dir.display()
        )
    })?;

    let path = manifest_path_for_dir(dir, &manifest.runtime_agent_id);
    let tmp_path = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(manifest).context("failed to serialize manifest")?;

    std::fs::write(&tmp_path, data)
        .with_context(|| format!("failed to write temp manifest {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &path)
        .with_context(|| format!("failed to install manifest {}", path.display()))?;
    Ok(())
}

pub fn remove_manifest(runtime_agent_id: &str) {
    if let Err(e) = std::fs::remove_file(manifest_path(runtime_agent_id)) {
        if e.kind() != std::io::ErrorKind::NotFound {
            warn!(
                "[runtime-agent-manifest] Failed to remove manifest for {}: {}",
                runtime_agent_id, e
            );
        }
    }
}

pub fn reap_orphaned_agents() -> usize {
    let mut reaped = 0;
    #[cfg(unix)]
    {
        reaped += reap_legacy_kernels_registry();
    }
    reaped += reap_legacy_agents_registry();
    reaped += reap_manifest_dir(&manifest_dir());
    reaped
}

pub(crate) fn reap_manifest_dir(dir: &Path) -> usize {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(e) => {
            warn!(
                "[runtime-agent-manifest] Failed to read manifest dir {}: {}",
                dir.display(),
                e
            );
            return 0;
        }
    };

    let mut reaped = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }

        let data = match std::fs::read_to_string(&path) {
            Ok(data) => data,
            Err(e) => {
                warn!(
                    "[runtime-agent-manifest] Failed to read manifest {}: {}",
                    path.display(),
                    e
                );
                continue;
            }
        };

        let manifest: RuntimeAgentManifest = match serde_json::from_str(&data) {
            Ok(manifest) => manifest,
            Err(e) => {
                warn!(
                    "[runtime-agent-manifest] Failed to parse manifest {}, retaining for inspection: {}",
                    path.display(),
                    e
                );
                continue;
            }
        };

        match cleanup_manifest(&manifest) {
            CleanupDecision::Reaped => {
                reaped += 1;
                remove_manifest_path(&path, &manifest);
            }
            CleanupDecision::Missing => {
                remove_manifest_path(&path, &manifest);
            }
            CleanupDecision::RetainForRetry => {}
        }
    }
    reaped
}

fn remove_manifest_path(path: &Path, manifest: &RuntimeAgentManifest) {
    if let Some(connection_file) = &manifest.connection_file {
        let _ = std::fs::remove_file(connection_file);
    }
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            warn!(
                "[runtime-agent-manifest] Failed to remove cleaned manifest {}: {}",
                path.display(),
                e
            );
        }
    }
}

fn cleanup_manifest(manifest: &RuntimeAgentManifest) -> CleanupDecision {
    if manifest.schema_version != SCHEMA_VERSION {
        warn!(
            "[runtime-agent-manifest] Unsupported manifest schema {} for {}, retaining",
            manifest.schema_version, manifest.runtime_agent_id
        );
        return CleanupDecision::RetainForRetry;
    }

    match manifest.platform {
        ManifestPlatform::Unix => cleanup_unix_manifest(manifest),
        ManifestPlatform::Windows => cleanup_windows_manifest(manifest),
    }
}

#[cfg(unix)]
fn cleanup_unix_manifest(manifest: &RuntimeAgentManifest) -> CleanupDecision {
    let Some(pgid) = manifest.pgid else {
        warn!(
            "[runtime-agent-manifest] Unix manifest {} has no pgid, retaining",
            manifest.runtime_agent_id
        );
        return CleanupDecision::RetainForRetry;
    };
    unix_cleanup_decision(
        &manifest.runtime_agent_id,
        pgid,
        kill_unix_process_group(pgid),
    )
}

#[cfg(not(unix))]
fn cleanup_unix_manifest(_manifest: &RuntimeAgentManifest) -> CleanupDecision {
    CleanupDecision::RetainForRetry
}

#[cfg(unix)]
fn kill_unix_process_group(pgid: i32) -> Result<(), nix::errno::Errno> {
    use nix::sys::signal::{killpg, Signal};
    use nix::unistd::Pid;

    if pgid <= 0 {
        return Err(nix::errno::Errno::EINVAL);
    }
    killpg(Pid::from_raw(pgid), Signal::SIGKILL)
}

#[cfg(unix)]
pub(crate) fn unix_cleanup_decision(
    runtime_agent_id: &str,
    pgid: i32,
    result: Result<(), nix::errno::Errno>,
) -> CleanupDecision {
    match result {
        Ok(()) => {
            info!(
                "[runtime-agent-manifest] Reaped runtime agent {} (pgid {})",
                runtime_agent_id, pgid
            );
            CleanupDecision::Reaped
        }
        Err(nix::errno::Errno::ESRCH) => {
            info!(
                "[runtime-agent-manifest] Runtime agent {} (pgid {}) already gone",
                runtime_agent_id, pgid
            );
            CleanupDecision::Missing
        }
        Err(nix::errno::Errno::EPERM) => {
            warn!(
                "[runtime-agent-manifest] Permission denied killing runtime agent {} (pgid {}), retaining manifest",
                runtime_agent_id, pgid
            );
            CleanupDecision::RetainForRetry
        }
        Err(e) => {
            tracing::error!(
                "[runtime-agent-manifest] Failed to kill runtime agent {} (pgid {}): {}",
                runtime_agent_id,
                pgid,
                e
            );
            CleanupDecision::RetainForRetry
        }
    }
}

#[cfg(windows)]
fn cleanup_windows_manifest(manifest: &RuntimeAgentManifest) -> CleanupDecision {
    let Some(pid) = manifest.pid else {
        warn!(
            "[runtime-agent-manifest] Windows manifest {} has no pid, retaining",
            manifest.runtime_agent_id
        );
        return CleanupDecision::RetainForRetry;
    };
    let Some(job_name) = manifest.job_name.as_deref() else {
        warn!(
            "[runtime-agent-manifest] Windows manifest {} has no job_name, retaining",
            manifest.runtime_agent_id
        );
        return CleanupDecision::RetainForRetry;
    };
    windows_cleanup_decision(
        &manifest.runtime_agent_id,
        pid,
        terminate_windows_runtime_agent(pid, job_name),
    )
}

#[cfg(not(windows))]
fn cleanup_windows_manifest(_manifest: &RuntimeAgentManifest) -> CleanupDecision {
    CleanupDecision::RetainForRetry
}

#[cfg(windows)]
#[derive(Debug)]
pub struct WindowsJob {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
pub fn windows_job_name(runtime_agent_id: &str) -> String {
    format!(
        "nteract-runtime-agent-{}",
        manifest_file_name(runtime_agent_id).trim_end_matches(".json")
    )
}

#[cfg(windows)]
pub fn create_windows_job_for_process(
    runtime_agent_id: &str,
    process_handle: std::os::windows::io::RawHandle,
) -> Result<(String, WindowsJob)> {
    use std::mem::{size_of, zeroed};
    use std::ptr::null;
    use windows_sys::Win32::Foundation::{GetLastError, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    let job_name = windows_job_name(runtime_agent_id);
    let wide_name = wide_null(&job_name);
    let job = unsafe { CreateJobObjectW(null(), wide_name.as_ptr()) };
    if job == 0 {
        anyhow::bail!("CreateJobObjectW({job_name}) failed: {}", unsafe {
            GetLastError()
        });
    }

    let job_handle = WindowsJob { handle: job };
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

    let set_ok = unsafe {
        SetInformationJobObject(
            job_handle.handle,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const _,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if set_ok == 0 {
        anyhow::bail!("SetInformationJobObject({job_name}) failed: {}", unsafe {
            GetLastError()
        });
    }

    let assign_ok =
        unsafe { AssignProcessToJobObject(job_handle.handle, process_handle as HANDLE) };
    if assign_ok == 0 {
        anyhow::bail!("AssignProcessToJobObject({job_name}) failed: {}", unsafe {
            GetLastError()
        });
    }

    Ok((job_name, job_handle))
}

#[cfg(windows)]
fn terminate_windows_runtime_agent(pid: u32, job_name: &str) -> Result<WindowsCleanupOutcome, u32> {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
    use windows_sys::Win32::System::JobObjects::{OpenJobObjectW, TerminateJobObject};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };

    const JOB_OBJECT_TERMINATE_ACCESS: u32 = 0x0008;

    let wide_name = wide_null(job_name);
    let job = unsafe { OpenJobObjectW(JOB_OBJECT_TERMINATE_ACCESS, 0, wide_name.as_ptr()) };
    if job != 0 {
        let ok = unsafe { TerminateJobObject(job, 1) };
        let err = unsafe { GetLastError() };
        unsafe {
            CloseHandle(job);
        }
        return if ok != 0 {
            Ok(WindowsCleanupOutcome::Reaped)
        } else {
            Err(err)
        };
    }

    let process = unsafe {
        OpenProcess(
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        )
    };
    if process == 0 {
        let err = unsafe { GetLastError() };
        return if err == windows_sys::Win32::Foundation::ERROR_INVALID_PARAMETER {
            Ok(WindowsCleanupOutcome::Missing)
        } else {
            Err(err)
        };
    }

    let ok = unsafe { TerminateProcess(process, 1) };
    let err = unsafe { GetLastError() };
    unsafe {
        CloseHandle(process);
    }
    if ok != 0 {
        Ok(WindowsCleanupOutcome::Reaped)
    } else {
        Err(err)
    }
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsCleanupOutcome {
    Reaped,
    Missing,
}

#[cfg(windows)]
pub fn windows_cleanup_decision(
    runtime_agent_id: &str,
    pid: u32,
    result: Result<WindowsCleanupOutcome, u32>,
) -> CleanupDecision {
    match result {
        Ok(WindowsCleanupOutcome::Reaped) => {
            info!(
                "[runtime-agent-manifest] Reaped runtime agent {} (pid {})",
                runtime_agent_id, pid
            );
            CleanupDecision::Reaped
        }
        Ok(WindowsCleanupOutcome::Missing) => {
            info!(
                "[runtime-agent-manifest] Runtime agent {} (pid {}) already gone",
                runtime_agent_id, pid
            );
            CleanupDecision::Missing
        }
        Err(err) => {
            warn!(
                "[runtime-agent-manifest] Failed to clean runtime agent {} (pid {}, win32 error {}), retaining manifest",
                runtime_agent_id, pid, err
            );
            CleanupDecision::RetainForRetry
        }
    }
}

#[cfg(windows)]
fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[derive(Debug, Serialize, Deserialize)]
struct LegacyAgentEntry {
    pgid: i32,
}

#[derive(Debug, Serialize, Deserialize)]
struct LegacyAgentRegistry {
    agents: HashMap<String, LegacyAgentEntry>,
}

fn reap_legacy_agents_registry() -> usize {
    reap_legacy_agents_registry_at_path(&legacy_agents_path(), legacy_unix_cleanup_decision)
}

fn reap_legacy_agents_registry_at_path(
    path: &Path,
    cleanup: impl Fn(&str, i32) -> CleanupDecision,
) -> usize {
    let data = match std::fs::read_to_string(path) {
        Ok(data) => data,
        Err(_) => return 0,
    };

    let registry: LegacyAgentRegistry = match serde_json::from_str(&data) {
        Ok(registry) => registry,
        Err(e) => {
            warn!(
                "[runtime-agent-manifest] Failed to parse legacy agents.json, retaining: {}",
                e
            );
            return 0;
        }
    };

    if registry.agents.is_empty() {
        let _ = std::fs::remove_file(path);
        return 0;
    }

    let mut reaped = 0;
    let mut failed = HashMap::new();
    for (agent_id, entry) in registry.agents {
        let decision = cleanup(&agent_id, entry.pgid);
        match decision {
            CleanupDecision::Reaped => reaped += 1,
            CleanupDecision::Missing => {}
            CleanupDecision::RetainForRetry => {
                failed.insert(agent_id, entry);
            }
        }
    }

    if failed.is_empty() {
        let _ = std::fs::remove_file(path);
    } else {
        let retained = LegacyAgentRegistry { agents: failed };
        match serde_json::to_vec_pretty(&retained) {
            Ok(data) => {
                if let Err(e) = std::fs::write(path, data) {
                    warn!(
                        "[runtime-agent-manifest] Failed to rewrite legacy agents.json: {}",
                        e
                    );
                }
            }
            Err(e) => warn!(
                "[runtime-agent-manifest] Failed to serialize retained legacy agents.json: {}",
                e
            ),
        }
    }
    reaped
}

#[cfg(unix)]
fn legacy_unix_cleanup_decision(agent_id: &str, pgid: i32) -> CleanupDecision {
    unix_cleanup_decision(agent_id, pgid, kill_unix_process_group(pgid))
}

#[cfg(not(unix))]
fn legacy_unix_cleanup_decision(_agent_id: &str, _pgid: i32) -> CleanupDecision {
    CleanupDecision::RetainForRetry
}

#[cfg(unix)]
#[derive(Deserialize)]
struct LegacyKernelRegistry {
    kernels: HashMap<String, LegacyAgentEntry>,
}

#[cfg(unix)]
fn reap_legacy_kernels_registry() -> usize {
    let path = legacy_kernels_path();
    let data = match std::fs::read_to_string(&path) {
        Ok(data) => data,
        Err(_) => return 0,
    };

    let registry: LegacyKernelRegistry = match serde_json::from_str(&data) {
        Ok(registry) => registry,
        Err(e) => {
            warn!(
                "[runtime-agent-manifest] Failed to parse legacy kernels.json, removing: {}",
                e
            );
            let _ = std::fs::remove_file(&path);
            return 0;
        }
    };

    let mut reaped = 0;
    for (kernel_id, entry) in registry.kernels {
        match legacy_unix_cleanup_decision(&kernel_id, entry.pgid) {
            CleanupDecision::Reaped => reaped += 1,
            CleanupDecision::Missing | CleanupDecision::RetainForRetry => {}
        }
    }
    let _ = std::fs::remove_file(&path);
    reaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_file_name_sanitizes_runtime_agent_id() {
        assert_eq!(
            manifest_file_name("runtime-agent:abc/def"),
            "runtime-agent_abc_def.json"
        );
    }

    #[test]
    fn manifest_path_uses_runtime_agents_directory() {
        let base = PathBuf::from("/tmp/runtimed-test");
        let dir = manifest_dir_for_base(&base);

        assert_eq!(
            manifest_path_for_dir(&dir, "runtime-agent:abcd"),
            base.join("runtime-agents/runtime-agent_abcd.json")
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_manifest_round_trips() {
        let manifest =
            RuntimeAgentManifest::unix("runtime-agent:abcd".into(), "notebook-1".into(), 42);

        let serialized = serde_json::to_string(&manifest).unwrap();
        let deserialized: RuntimeAgentManifest = serde_json::from_str(&serialized).unwrap();

        assert_eq!(deserialized.schema_version, SCHEMA_VERSION);
        assert_eq!(deserialized.platform, ManifestPlatform::Unix);
        assert_eq!(deserialized.pgid, Some(42));
        assert_eq!(deserialized.pid, None);
        assert_eq!(deserialized.job_name, None);
    }

    #[test]
    fn windows_shaped_manifest_round_trips() {
        let manifest = RuntimeAgentManifest {
            schema_version: SCHEMA_VERSION,
            runtime_agent_id: "runtime-agent:abcd".into(),
            notebook_id: "notebook-1".into(),
            daemon_pid: 99,
            created_at_unix_ms: 123,
            connection_file: None,
            platform: ManifestPlatform::Windows,
            pgid: None,
            pid: Some(1234),
            job_name: Some("nteract-runtime-agent-runtime-agent_abcd".into()),
        };

        let serialized = serde_json::to_string(&manifest).unwrap();
        let deserialized: RuntimeAgentManifest = serde_json::from_str(&serialized).unwrap();

        assert_eq!(deserialized.platform, ManifestPlatform::Windows);
        assert_eq!(deserialized.pid, Some(1234));
        assert_eq!(
            deserialized.job_name.as_deref(),
            Some("nteract-runtime-agent-runtime-agent_abcd")
        );
    }

    #[test]
    fn legacy_agents_json_removes_successful_and_missing_entries() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("agents.json");
        std::fs::write(
            &path,
            r#"{"agents":{"gone":{"pgid":1},"killed":{"pgid":2}}}"#,
        )
        .unwrap();

        let reaped = reap_legacy_agents_registry_at_path(&path, |id, _| match id {
            "killed" => CleanupDecision::Reaped,
            "gone" => CleanupDecision::Missing,
            _ => unreachable!(),
        });

        assert_eq!(reaped, 1);
        assert!(!path.exists());
    }

    #[test]
    fn legacy_agents_json_retains_failed_entries_only() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("agents.json");
        std::fs::write(
            &path,
            r#"{"agents":{"denied":{"pgid":1},"gone":{"pgid":2}}}"#,
        )
        .unwrap();

        let reaped = reap_legacy_agents_registry_at_path(&path, |id, _| match id {
            "denied" => CleanupDecision::RetainForRetry,
            "gone" => CleanupDecision::Missing,
            _ => unreachable!(),
        });

        assert_eq!(reaped, 0);
        let retained: LegacyAgentRegistry =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(retained.agents.len(), 1);
        assert!(retained.agents.contains_key("denied"));
    }

    #[cfg(unix)]
    #[test]
    fn unix_cleanup_decision_retains_permission_denied_entries() {
        let decision = unix_cleanup_decision("agent-1", 1234, Err(nix::errno::Errno::EPERM));

        assert_eq!(decision, CleanupDecision::RetainForRetry);
    }

    #[cfg(unix)]
    #[test]
    fn unix_cleanup_decision_removes_entries_for_missing_processes() {
        let decision = unix_cleanup_decision("agent-1", 1234, Err(nix::errno::Errno::ESRCH));

        assert_eq!(decision, CleanupDecision::Missing);
    }

    #[cfg(unix)]
    #[test]
    fn unix_cleanup_decision_counts_successful_kills_as_reaped() {
        let decision = unix_cleanup_decision("agent-1", 1234, Ok(()));

        assert_eq!(decision, CleanupDecision::Reaped);
    }

    #[cfg(unix)]
    #[test]
    fn reap_manifest_dir_removes_manifest_for_short_lived_process_group() {
        use std::os::unix::process::CommandExt;

        let temp = tempfile::tempdir().unwrap();
        let dir = manifest_dir_for_base(temp.path());
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .process_group(0)
            .spawn()
            .unwrap();
        let pgid = child.id() as i32;
        let manifest = RuntimeAgentManifest::unix("runtime-agent:test".into(), "nb".into(), pgid);
        write_manifest_to_dir(&dir, &manifest).unwrap();

        assert_eq!(reap_manifest_dir(&dir), 1);
        assert!(!manifest_path_for_dir(&dir, "runtime-agent:test").exists());
        let _ = child.wait();
    }
}
