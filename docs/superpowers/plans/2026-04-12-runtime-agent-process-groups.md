# Runtime Agent Process Groups Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make runtime agent subprocesses use process groups for clean orphan reaping, replacing the per-kernel PGID tracking with per-agent PGID tracking.

**Architecture:** The runtime agent becomes a process group leader (`process_group(0)`), its kernel inherits the PGID. A renamed registry (`agents.json`) tracks agent PGIDs for orphan reaping on daemon restart. The kernel no longer owns a process group — it's killed by PID during shutdown/restart.

**Tech Stack:** Rust, tokio, nix (Unix signals/process groups), serde_json (registry persistence)

**Spec:** `docs/superpowers/specs/2026-04-12-runtime-agent-process-groups-design.md`

---

### Task 1: Rename kernel_pids.rs to process_groups.rs

Rename the module, types, functions, and persistence file. Add backward-compat migration for `kernels.json`.

**Files:**
- Rename: `crates/runtimed/src/kernel_pids.rs` → `crates/runtimed/src/process_groups.rs`
- Modify: `crates/runtimed/src/lib.rs:29`
- Modify: `crates/runtimed/src/daemon.rs:701`
- Modify: `crates/runtimed/src/jupyter_kernel.rs:444,1956,2231`

- [ ] **Step 1: Rename the file**

```bash
git mv crates/runtimed/src/kernel_pids.rs crates/runtimed/src/process_groups.rs
```

- [ ] **Step 2: Rename types and functions inside process_groups.rs**

Replace the module doc comment, struct names, function names, log prefixes, and persistence filename:

```rust
//! Persist runtime agent process group IDs to disk for orphan reaping.
//!
//! When runtimed is killed with SIGKILL, its graceful shutdown code never runs.
//! Runtime agents spawned with `process_group(0)` survive as orphans along with
//! their kernel subprocesses. This module persists agent pgids to `agents.json`
//! so the next daemon instance can reap them on startup.
//!
//! Connection files are colocated in `connections_dir()` alongside this registry.
//! Together they contain enough information to kill orphaned agent process groups.
//!
//! All I/O is synchronous (`std::fs`) since it's called from both async and
//! `Drop` contexts. The registry is only accessed by a single daemon process,
//! so no file locking is needed — Tokio's cooperative scheduling ensures the
//! synchronous read-modify-write won't interleave between tasks.
```

Rename structs:
- `KernelEntry` → `AgentEntry`
- `KernelRegistry` → `AgentRegistry`
- `KernelRegistry { kernels: ... }` → `AgentRegistry { agents: ... }`

Rename functions:
- `register_kernel()` → `register_agent()`
- `unregister_kernel()` → `unregister_agent()`
- `reap_orphaned_kernels()` → `reap_orphaned_agents()`

Rename persistence file:
- `registry_path()` returns `daemon_base_dir().join("agents.json")`

Update all log prefixes from `[kernel-pids]` to `[process-groups]`.

- [ ] **Step 3: Add legacy kernels.json migration to reap_orphaned_agents()**

At the start of `reap_orphaned_agents()`, before reading `agents.json`, check for and reap a leftover `kernels.json`:

```rust
/// Path to the legacy registry from before the kernel→agent migration.
fn legacy_registry_path() -> PathBuf {
    daemon_base_dir().join("kernels.json")
}

/// Reap any leftover kernel PGIDs from the legacy `kernels.json` and delete it.
#[cfg(unix)]
fn reap_legacy_registry() -> usize {
    let path = legacy_registry_path();
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(_) => return 0,
    };

    // The legacy format is identical: { "kernels": { "id": { "pgid": N } } }
    #[derive(Deserialize)]
    struct LegacyRegistry {
        kernels: HashMap<String, AgentEntry>,
    }

    let registry: LegacyRegistry = match serde_json::from_str(&data) {
        Ok(r) => r,
        Err(_) => {
            let _ = std::fs::remove_file(&path);
            return 0;
        }
    };

    let mut reaped = 0;
    for (id, entry) in &registry.kernels {
        use nix::sys::signal::{killpg, Signal};
        use nix::unistd::Pid;

        if entry.pgid <= 0 {
            continue;
        }
        match killpg(Pid::from_raw(entry.pgid), Signal::SIGKILL) {
            Ok(()) => {
                info!("[process-groups] Reaped legacy kernel '{}' (pgid {})", id, entry.pgid);
                reaped += 1;
            }
            Err(nix::errno::Errno::ESRCH) => {}
            Err(e) => {
                warn!("[process-groups] Failed to reap legacy kernel '{}' (pgid {}): {}", id, entry.pgid, e);
            }
        }
    }

    let _ = std::fs::remove_file(&path);
    reaped
}
```

Call it at the top of `reap_orphaned_agents()`:

```rust
#[cfg(unix)]
pub fn reap_orphaned_agents() -> usize {
    let legacy_reaped = reap_legacy_registry();

    let registry = match read_registry() {
        // ... rest unchanged, just with renamed types ...
    };
    // ...
    reaped + legacy_reaped
}
```

- [ ] **Step 4: Update all call sites to use new module name**

In `crates/runtimed/src/lib.rs:29`:
```rust
pub mod process_groups;
```

In `crates/runtimed/src/daemon.rs:701`:
```rust
let reaped = crate::process_groups::reap_orphaned_agents();
```
Update the log message on line 704 to say "agent process group(s)".

In `crates/runtimed/src/jupyter_kernel.rs:444`:
```rust
crate::process_groups::register_agent(&kernel_id, pgid);
```

In `crates/runtimed/src/jupyter_kernel.rs:1956`:
```rust
crate::process_groups::unregister_agent(&kid);
```

In `crates/runtimed/src/jupyter_kernel.rs:2231`:
```rust
crate::process_groups::unregister_agent(&kid);
```

(These three kernel call sites will be removed in Task 3, but we need them to compile here.)

- [ ] **Step 5: Build and test**

```bash
cargo build -p runtimed 2>&1 | tail -5
cargo test -p runtimed --lib 2>&1 | tail -5
```

Expected: clean build, all tests pass.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor(runtimed): rename kernel_pids to process_groups

Rename module, types, functions, and persistence file (kernels.json →
agents.json) to reflect that we now track runtime agent PGIDs, not
kernel PGIDs. Add backward-compat migration to reap leftover
kernels.json from previous versions."
```

---

### Task 2: Add process group and PGID tracking to RuntimeAgentHandle

Make the runtime agent a process group leader, track its PGID in the registry, and add cleanup on drop.

**Files:**
- Modify: `crates/runtimed/src/runtime_agent_handle.rs`

- [ ] **Step 1: Add process group, PGID field, and registry integration**

Replace the full `runtime_agent_handle.rs` with:

```rust
//! Coordinator-side management of a runtime agent subprocess.
//!
//! `RuntimeAgentHandle` spawns a `runtimed runtime-agent` child process in its
//! own process group. The PGID is registered in `agents.json` for orphan
//! reaping. The handle monitors the child lifecycle and sends SIGKILL to the
//! entire process group on drop.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use tracing::{info, warn};

/// Handle to a running runtime agent subprocess.
///
/// The runtime agent connects back to the daemon's Unix socket as a peer.
/// On drop, sends SIGKILL to the agent's process group (agent + kernel).
pub struct RuntimeAgentHandle {
    alive: Arc<AtomicBool>,
    /// Notebook ID used as the registry key for orphan tracking.
    notebook_id: String,
    /// Process group ID (== agent PID on Unix, since we use process_group(0)).
    #[cfg(unix)]
    pgid: Option<i32>,
}

impl RuntimeAgentHandle {
    /// Spawn a runtime agent subprocess that will connect back to the daemon socket.
    ///
    /// The runtime agent is given the socket path, notebook ID, runtime agent ID,
    /// and blob root as CLI arguments. It connects to the daemon socket and joins
    /// the notebook room as a `RuntimeAgent` peer.
    pub async fn spawn(
        notebook_id: String,
        runtime_agent_id: String,
        blob_root: PathBuf,
        socket_path: PathBuf,
    ) -> Result<Self> {
        let exe = std::env::current_exe()?;
        info!(
            "[runtime-agent-handle] Spawning runtime agent: {} runtime-agent --notebook-id {} (socket: {})",
            exe.display(),
            notebook_id,
            socket_path.display(),
        );

        let mut cmd = tokio::process::Command::new(&exe);
        cmd.arg("runtime-agent")
            .arg("--notebook-id")
            .arg(&notebook_id)
            .arg("--runtime-agent-id")
            .arg(&runtime_agent_id)
            .arg("--blob-root")
            .arg(blob_root.as_os_str())
            .arg("--socket")
            .arg(socket_path.as_os_str())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);

        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd.spawn()?;

        #[cfg(unix)]
        let pgid = child.id().map(|pid| pid as i32);

        // Register PGID for orphan reaping on unclean daemon exit
        #[cfg(unix)]
        if let Some(pgid) = pgid {
            crate::process_groups::register_agent(&notebook_id, pgid);
        }

        info!(
            "[runtime-agent-handle] Runtime agent spawned (pid={:?}, notebook_id={})",
            child.id(),
            notebook_id,
        );

        let alive = Arc::new(AtomicBool::new(true));

        // Monitor child process exit
        let alive_clone = alive.clone();
        let runtime_agent_id_clone = runtime_agent_id.clone();
        tokio::spawn(async move {
            match child.wait().await {
                Ok(status) => {
                    info!(
                        "[runtime-agent-handle] Runtime agent {} exited with status: {}",
                        runtime_agent_id_clone, status
                    );
                }
                Err(e) => {
                    warn!(
                        "[runtime-agent-handle] Runtime agent {} wait error: {}",
                        runtime_agent_id_clone, e
                    );
                }
            }
            alive_clone.store(false, Ordering::Relaxed);
        });

        Ok(Self {
            alive,
            notebook_id,
            #[cfg(unix)]
            pgid,
        })
    }

    /// Check if the runtime agent process is still running.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// Unregister this agent from the process group registry.
    ///
    /// Called during graceful shutdown and room eviction, before
    /// the handle is dropped.
    pub fn unregister(&self) {
        crate::process_groups::unregister_agent(&self.notebook_id);
    }
}

impl Drop for RuntimeAgentHandle {
    fn drop(&mut self) {
        // SIGKILL the entire process group (agent + kernel)
        #[cfg(unix)]
        if let Some(pgid) = self.pgid.take() {
            use nix::sys::signal::{killpg, Signal};
            use nix::unistd::Pid;
            let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
        }

        info!(
            "[runtime-agent-handle] RuntimeAgentHandle dropped for notebook {}",
            self.notebook_id
        );
    }
}
```

- [ ] **Step 2: Build and test**

```bash
cargo build -p runtimed 2>&1 | tail -5
cargo test -p runtimed --lib 2>&1 | tail -5
```

Expected: clean build, all tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/runtimed/src/runtime_agent_handle.rs
git commit -m "feat(runtimed): add process group and PGID tracking to RuntimeAgentHandle

Runtime agent now spawns with process_group(0), registers its PGID in
agents.json for orphan reaping, and sends SIGKILL to the entire process
group on drop."
```

---

### Task 3: Remove process group ownership from JupyterKernel

The kernel no longer owns a process group. Replace `killpg()` with per-PID `kill()` for kernel lifecycle. Remove registry calls.

**Files:**
- Modify: `crates/runtimed/src/jupyter_kernel.rs`

- [ ] **Step 1: Add kernel_pid field, remove process_group_id and kernel_id fields**

In the `JupyterKernel` struct (line 56), replace:

```rust
    /// Process group ID for cleanup (Unix only).
    #[cfg(unix)]
    process_group_id: Option<i32>,
    /// Kernel ID for the process registry (orphan reaping).
    kernel_id: Option<String>,
```

with:

```rust
    /// Kernel process PID for signal-based cleanup (Unix only).
    #[cfg(unix)]
    kernel_pid: Option<i32>,
```

- [ ] **Step 2: Update launch() — remove process_group(0), remove register_kernel, store PID**

At line 418-419, remove:
```rust
        #[cfg(unix)]
        cmd.process_group(0);
```

At lines 440-445, replace:
```rust
        #[cfg(unix)]
        let process_group_id = process.id().map(|pid| pid as i32);
        #[cfg(unix)]
        if let Some(pgid) = process_group_id {
            crate::process_groups::register_agent(&kernel_id, pgid);
        }
```

with:

```rust
        #[cfg(unix)]
        let kernel_pid = process.id().map(|pid| pid as i32);
```

- [ ] **Step 3: Update the struct literal in the connect/setup path**

Find where the `JupyterKernel` struct is constructed (around line 1780-1810). Replace `process_group_id` and `kernel_id` fields with `kernel_pid`:

Replace:
```rust
            #[cfg(unix)]
            process_group_id,
            kernel_id: Some(kernel_id),
```

with:

```rust
            #[cfg(unix)]
            kernel_pid,
```

- [ ] **Step 4: Replace killpg with kill-by-PID in graceful shutdown**

In the `shutdown_kernel()` method (around line 1906), replace the process group signal block:

```rust
        #[cfg(unix)]
        if let Some(pgid) = self.process_group_id.take() {
            use nix::sys::signal::{killpg, Signal};
            use nix::unistd::Pid;

            // Wait up to 2s for the kernel to exit after shutdown_request
            if !wait_for_process_group_exit(pgid, std::time::Duration::from_secs(2)).await {
                // SIGTERM: politely ask the process group to terminate
                info!(
                    "[jupyter-kernel] Kernel didn't exit after shutdown_request, sending SIGTERM to pgid {}",
                    pgid
                );
                let _ = killpg(Pid::from_raw(pgid), Signal::SIGTERM);

                // Wait up to 3s for SIGTERM
                if !wait_for_process_group_exit(pgid, std::time::Duration::from_secs(3)).await {
                    // SIGKILL: force kill as last resort
                    info!(
                        "[jupyter-kernel] Kernel didn't respond to SIGTERM, sending SIGKILL to pgid {}",
                        pgid
                    );
                    if let Err(e) = killpg(Pid::from_raw(pgid), Signal::SIGKILL) {
                        match e {
                            nix::errno::Errno::ESRCH => {}
                            other => {
                                debug!(
                                    "[jupyter-kernel] Failed to SIGKILL process group {}: {}",
                                    pgid, other
                                );
                            }
                        }
                    }
                }
            }
        }
```

with:

```rust
        #[cfg(unix)]
        if let Some(pid) = self.kernel_pid.take() {
            use nix::sys::signal::{kill, Signal};
            use nix::unistd::Pid;

            // Wait up to 2s for the kernel to exit after shutdown_request
            if !wait_for_pid_exit(pid, std::time::Duration::from_secs(2)).await {
                info!(
                    "[jupyter-kernel] Kernel didn't exit after shutdown_request, sending SIGTERM to pid {}",
                    pid
                );
                let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);

                // Wait up to 3s for SIGTERM
                if !wait_for_pid_exit(pid, std::time::Duration::from_secs(3)).await {
                    info!(
                        "[jupyter-kernel] Kernel didn't respond to SIGTERM, sending SIGKILL to pid {}",
                        pid
                    );
                    if let Err(e) = kill(Pid::from_raw(pid), Signal::SIGKILL) {
                        match e {
                            nix::errno::Errno::ESRCH => {}
                            other => {
                                debug!(
                                    "[jupyter-kernel] Failed to SIGKILL kernel pid {}: {}",
                                    pid, other
                                );
                            }
                        }
                    }
                }
            }
        }
```

Remove the `unregister_agent` call that was on lines 1955-1957:
```rust
        if let Some(kid) = self.kernel_id.take() {
            crate::process_groups::unregister_agent(&kid);
        }
```

- [ ] **Step 5: Replace killpg with kill-by-PID in Drop impl**

Replace:
```rust
        // Kill process group on Unix
        #[cfg(unix)]
        if let Some(pgid) = self.process_group_id.take() {
            use nix::sys::signal::{killpg, Signal};
            use nix::unistd::Pid;
            let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
        }

        // Unregister from process registry
        if let Some(kid) = self.kernel_id.take() {
            crate::process_groups::unregister_agent(&kid);
        }
```

with:

```rust
        // Kill kernel process on Unix
        #[cfg(unix)]
        if let Some(pid) = self.kernel_pid.take() {
            use nix::sys::signal::{kill, Signal};
            use nix::unistd::Pid;
            let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
        }
```

- [ ] **Step 6: Rename wait_for_process_group_exit to wait_for_pid_exit**

Replace `wait_for_process_group_exit` (around line 2245) with:

```rust
#[cfg(unix)]
async fn wait_for_pid_exit(pid: i32, timeout: std::time::Duration) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match kill(Pid::from_raw(pid), None) {
            Err(nix::errno::Errno::ESRCH) => return true,
            Err(_) => return true,
            Ok(()) => {
                if tokio::time::Instant::now() >= deadline {
                    return false;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}
```

- [ ] **Step 7: Fix all remaining references to removed fields**

Search for any remaining references to `process_group_id`, `kernel_id` (the field, not the local variable), `register_agent`, and `unregister_agent` in `jupyter_kernel.rs`. The local `kernel_id` variable used for logging and connection files should remain — only the struct field and registry calls are removed.

```bash
grep -n 'process_group_id\|kernel_id.*take\|register_agent\|unregister_agent' crates/runtimed/src/jupyter_kernel.rs
```

Expected: no matches (all removed).

- [ ] **Step 8: Build and test**

```bash
cargo build -p runtimed 2>&1 | tail -5
cargo test -p runtimed --lib 2>&1 | tail -5
cargo test -p runtimed --test tokio_mutex_lint 2>&1 | tail -5
```

Expected: clean build, all tests pass.

- [ ] **Step 9: Commit**

```bash
git add crates/runtimed/src/jupyter_kernel.rs
git commit -m "refactor(runtimed): remove process group ownership from JupyterKernel

Kernel no longer creates its own process group — it inherits the runtime
agent's PGID. Replace killpg() with per-PID kill() for kernel shutdown
and restart. Remove kernel PGID registration since the agent handles
orphan tracking."
```

---

### Task 4: Add unregister_agent calls to daemon shutdown and room eviction

Ensure graceful paths clean up the registry.

**Files:**
- Modify: `crates/runtimed/src/daemon.rs:773-801`
- Modify: `crates/runtimed/src/notebook_sync_server.rs:2073-2112`

- [ ] **Step 1: Add unregister in daemon shutdown**

In `daemon.rs`, inside the shutdown loop (around line 773), after the `ShutdownKernel` RPC and before clearing the handle, add the unregister call:

```rust
        for (notebook_id, room) in drained_rooms {
            // Shut down runtime agent via RPC before dropping handle
            {
                let has_runtime_agent = room.runtime_agent_request_tx.lock().await.is_some();
                if has_runtime_agent {
                    info!(
                        "[runtimed] Shutting down runtime agent for notebook on exit: {}",
                        notebook_id
                    );
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        crate::notebook_sync_server::send_runtime_agent_request(
                            &room,
                            notebook_protocol::protocol::RuntimeAgentRequest::ShutdownKernel,
                        ),
                    )
                    .await;
                }
                // Unregister from process group registry before dropping handle
                {
                    let ra_guard = room.runtime_agent_handle.lock().await;
                    if let Some(ref handle) = *ra_guard {
                        handle.unregister();
                    }
                }
                // Scope each lock independently to avoid cross-lock ordering.
                {
                    let mut ra_guard = room.runtime_agent_handle.lock().await;
                    *ra_guard = None;
                }
                {
                    let mut tx = room.runtime_agent_request_tx.lock().await;
                    *tx = None;
                }
            }
        }
```

- [ ] **Step 2: Add unregister in room eviction**

In `notebook_sync_server.rs`, in the eviction teardown (around line 2083), after the shutdown RPC timeout block and before clearing the handle:

```rust
                    if has_runtime_agent {
                        // ... existing shutdown RPC timeout block ...

                        // Unregister from process group registry before dropping handle
                        {
                            let ra_guard = room_for_eviction.runtime_agent_handle.lock().await;
                            if let Some(ref handle) = *ra_guard {
                                handle.unregister();
                            }
                        }
                        // Scope each lock independently to avoid cross-lock ordering.
                        {
                            let mut guard = room_for_eviction.runtime_agent_handle.lock().await;
                            *guard = None;
                        }
                        {
                            let mut tx = room_for_eviction.runtime_agent_request_tx.lock().await;
                            *tx = None;
                        }
                    }
```

- [ ] **Step 3: Update daemon shutdown comment**

Update the comment block at line 755-764 to reflect the new process model:

```rust
        // Shut down all runtime agents before exiting.
        //
        // Runtime agents are spawned in their own process group (process_group(0)),
        // so they do NOT receive the SIGINT/SIGTERM that the daemon receives.
        // Their kernel subprocesses inherit the agent's PGID, so killing the
        // agent's process group kills the kernel too.
        //
        // Without explicit shutdown here, agent process groups become orphans.
        // We cannot rely on Drop alone because:
        //   1. The runtime agent handle is behind Arc<Mutex<Option<...>>> inside
        //      Arc<NotebookRoom> — multiple spawned tasks hold Arc clones that
        //      may not all unwind during tokio runtime teardown.
        //   2. A second ctrl-c or SIGKILL skips destructors entirely.
        //
        // To avoid holding the notebook_rooms lock across .await points, first
        // drain the map into an owned collection, then shut down agents.
```

- [ ] **Step 4: Build and test**

```bash
cargo build -p runtimed 2>&1 | tail -5
cargo test -p runtimed --lib 2>&1 | tail -5
```

Expected: clean build, all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/runtimed/src/daemon.rs crates/runtimed/src/notebook_sync_server.rs
git commit -m "fix(runtimed): add unregister_agent calls to daemon shutdown and room eviction

Previously, graceful shutdown and room eviction didn't unregister from
the process group registry, leaving stale entries that would trigger
false-positive reaping on daemon restart."
```

---

### Task 5: Lint, full build, and verify

Run formatting, full workspace build, and all relevant tests.

**Files:** None (verification only)

- [ ] **Step 1: Lint**

```bash
cargo xtask lint --fix 2>&1 | tail -15
```

Expected: all checks passed.

- [ ] **Step 2: Full build**

```bash
cargo build --workspace --exclude runtimed-py 2>&1 | tail -5
```

Expected: clean build.

- [ ] **Step 3: Run all runtimed tests**

```bash
cargo test -p runtimed --lib 2>&1 | tail -5
cargo test -p runtimed --test tokio_mutex_lint 2>&1 | tail -5
cargo test -p runt-cli 2>&1 | tail -5
cargo test -p notebook --lib 2>&1 | tail -5
```

Expected: all pass.

- [ ] **Step 4: Verify no leftover references to old names**

```bash
grep -rn 'kernel_pids\|register_kernel\|unregister_kernel\|reap_orphaned_kernels\|KernelEntry\|KernelRegistry\|kernels\.json' crates/runtimed/src/ --include='*.rs'
```

Expected: no matches. (The only `kernels.json` reference should be in `process_groups.rs` inside `legacy_registry_path()`.)

- [ ] **Step 5: Verify grep for process_group_id in jupyter_kernel.rs**

```bash
grep -n 'process_group_id' crates/runtimed/src/jupyter_kernel.rs
```

Expected: no matches.

- [ ] **Step 6: Commit any lint fixes**

```bash
git add -A
git commit -m "style: lint fixes" || echo "nothing to commit"
```
