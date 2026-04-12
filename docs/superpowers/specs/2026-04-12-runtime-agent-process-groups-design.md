# Runtime Agent Process Groups

**Issue:** #1714
**Date:** 2026-04-12
**Status:** Design approved

## Problem

Runtime agents spawn without `.process_group(0)` and aren't tracked in any
registry. When the daemon dies uncleanly (SIGKILL, crash), runtime agents and
their kernels become orphans. `.kill_on_drop()` doesn't fire during SIGKILL.
Result: zombie `runtimed` processes accumulate (100+ observed after unclean
exits).

The current PID tracking infrastructure (`kernel_pids.rs` / `kernels.json`)
tracks kernel PGIDs but not the runtime agents that own them. This is a
remnant of an earlier architecture where kernels ran inside the daemon process.

## Process Tree: Before and After

**Before** (each kernel is its own process group, agent untracked):

```
runtimed (daemon)
  ├── runtime-agent  (no process group, no tracking)
  │   └── kernel     (own PGID, tracked in kernels.json)
  └── runtime-agent
      └── kernel
```

**After** (agent is the process group leader, kernel inherits):

```
runtimed (daemon)
  ├── runtime-agent  (PGID leader, tracked in agents.json)
  │   └── kernel     (inherits agent PGID)
  └── runtime-agent
      └── kernel
```

One runtime agent manages exactly one kernel. This is a 1:1 relationship and
the design does not accommodate multi-kernel agents.

## Changes

### 1. runtime_agent_handle.rs — Process group and PGID tracking

Add `.process_group(0)` to the runtime agent `Command` so the agent becomes a
process group leader. Capture the PGID after spawn and register it in the new
`process_groups` registry.

Store the PGID on `RuntimeAgentHandle` for use during graceful shutdown
(SIGTERM to group, then SIGKILL fallback). Add a `Drop` impl that sends
SIGKILL to the process group as a last resort.

Expose a method to unregister the agent from the process group registry (called
during graceful shutdown and room eviction).

### 2. jupyter_kernel.rs — Remove process group ownership

The kernel no longer owns a process group. It inherits the runtime agent's
group.

Remove:
- `.process_group(0)` from the kernel `Command`
- `process_group_id` field from `JupyterKernel`
- `register_kernel()` / `unregister_kernel()` calls
- `killpg()` calls from graceful shutdown and `Drop` impl

Replace `killpg(pgid, signal)` with per-PID `kill(pid, signal)` for kernel
lifecycle management. The graceful shutdown path (ZMQ `ShutdownRequest`) stays
as-is. Fallback: SIGTERM then SIGKILL to the kernel's PID.

This is a simplification: `JupyterKernel` no longer needs to know about
process groups at all. It just manages a child process by PID.

### 3. kernel_pids.rs → process_groups.rs — Registry rename

Rename the file and all internal types to reflect what's actually tracked:

| Before | After |
|--------|-------|
| `kernel_pids.rs` | `process_groups.rs` |
| `KernelEntry` | `AgentEntry` |
| `KernelRegistry` | `AgentRegistry` |
| `kernels.json` | `agents.json` |
| `register_kernel()` | `register_agent()` |
| `unregister_kernel()` | `unregister_agent()` |
| `reap_orphaned_kernels()` | `reap_orphaned_agents()` |

Add backward compatibility: `reap_orphaned_agents()` checks for and reaps any
leftover `kernels.json` from before the migration, then deletes it.

Connection file cleanup stays in the reaper as a safety net for unclean exits.

### 4. daemon.rs — Startup and shutdown integration

**Startup:** Call `reap_orphaned_agents()` instead of `reap_orphaned_kernels()`.

**Shutdown:** After sending `ShutdownKernel` RPC to each room's runtime agent,
call `unregister_agent()` to remove the PGID from the registry. This is
currently missing even for kernel PIDs.

### 5. notebook_sync_server.rs — Room eviction

Add `unregister_agent()` call during room teardown. Currently missing — rooms
that are evicted while their runtime agent is running leave stale entries in the
registry.

### 6. Kernel restart

Kernel restart (kill old kernel, start new one) works without re-registration
because the runtime agent's PGID doesn't change. The kernel is killed by PID
(not PGID), so the agent survives the restart.

- Old kernel: `kill(pid, SIGTERM)` then `kill(pid, SIGKILL)` fallback
- New kernel: spawned normally, inherits agent's process group
- No registry updates needed — the agent entry covers both

## What stays the same

- Graceful shutdown path: daemon sends `ShutdownKernel` RPC to runtime agent,
  agent sends ZMQ `ShutdownRequest` to kernel, kernel exits, agent exits
- Connection file creation (in `jupyter_kernel.rs`)
- Connection file cleanup in reaper (safety net for crashes)
- `.kill_on_drop(true)` on both agent and kernel commands (belt and suspenders)

## Verification

1. Launch a notebook, run `ps -eo pid,pgid,comm | grep runtimed` — confirm
   runtime agent and kernel share a PGID, different from the daemon's
2. SIGKILL the daemon, restart — confirm `reap_orphaned_agents()` kills the
   orphaned process group and cleans up `agents.json`
3. Graceful shutdown (daemon stop) — confirm `ShutdownKernel` RPC path still
   works, no orphans left
4. Kernel restart — confirm old kernel dies by PID, new kernel starts, agent
   stays alive, same PGID throughout
5. Room eviction — confirm agent PGID is unregistered from `agents.json`
6. Migration — start daemon with leftover `kernels.json` from old version,
   confirm it's reaped and deleted
