# Protocol Security Audit

**Date**: 2026-03-07
**Scope**: All IPC, sync, trust, and network protocols in the nteract/desktop codebase

## Executive Summary

The codebase implements six major protocol surfaces: daemon IPC (Unix sockets / named pipes), Automerge notebook sync, HMAC-SHA256 dependency trust, a localhost blob HTTP server, Jupyter kernel wire protocol, and a singleton lock mechanism. Overall the protocol design is solid, with several good security practices already in place (constant-time HMAC comparison, content-addressed blob hashing with strict validation, SHA-256 hashed persistence filenames). This audit identifies areas for hardening and improvement.

---

## 1. Trust Protocol (HMAC-SHA256 Dependency Signing)

**Files**: `crates/runt-trust/src/lib.rs`

### What it does

Signs notebook dependency metadata (`metadata.runt.uv`, `metadata.runt.conda`) with a per-machine HMAC-SHA256 key stored at `~/.config/runt/trust-key`. Untrusted notebooks prompt the user before installing packages.

### Strengths

- **Constant-time comparison**: Uses `mac.verify_slice()` (line 186), which performs constant-time comparison, preventing timing attacks.
- **Signs only dependency metadata**: Cell edits don't invalidate trust, reducing user friction while maintaining supply-chain protection.
- **Per-machine keys**: Keys never leave the machine, preventing cross-machine signature forgery.
- **Typosquat detection**: Separate module (`crates/notebook/src/typosquat.rs`) warns about packages similar to popular ones.

### Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **Medium** | **Trust key file has no restricted permissions.** The key is written with `std::fs::write()` (line 93) which uses the default umask — typically `0644`, making the key world-readable. Should be `0600`. | `runt-trust/src/lib.rs:93` |
| **Low** | **No key rotation mechanism.** If the key is compromised, all previously signed notebooks remain trusted. A key rotation feature (re-sign all notebooks with new key) would limit blast radius. | `runt-trust/src/lib.rs:69-97` |
| **Low** | **`extract_signable_content` uses `serde_json::to_string` for canonicalization.** `serde_json` does not guarantee key ordering for `serde_json::Map` (it preserves insertion order). If the metadata HashMap iteration order changes between sign and verify, signatures could break. In practice this works because the function constructs a fresh `serde_json::Map` with `insert()` in a fixed order (`"conda"`, `"uv"`), but this is fragile. | `runt-trust/src/lib.rs:106-121` |
| **Info** | **`RUNT_TRUST_KEY_PATH` env override could be used in production.** The environment variable is intended for testing but is unconditionally checked. An attacker who can set environment variables could redirect trust verification to a key they control. This requires local code execution, so it's low risk. | `runt-trust/src/lib.rs:59` |

### Recommendation

```rust
// After writing the key file, restrict permissions (Unix)
#[cfg(unix)]
{
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("Failed to set trust key permissions: {}", e))?;
}
```

---

## 2. Daemon IPC Protocol (Unix Socket / Named Pipe)

**Files**: `crates/runtimed/src/connection.rs`, `crates/runtimed/src/daemon.rs`

### What it does

The daemon listens on a Unix domain socket (`~/.cache/runt/runtimed.sock`) or Windows named pipe. All channels (pool, settings sync, notebook sync, blob) are multiplexed over this single socket using a JSON handshake that declares the channel type, followed by length-prefixed binary frames.

### Strengths

- **Unix sockets have filesystem-level access control**: Only the owning user can connect (enforced by parent directory permissions in `~/.cache/runt/`).
- **Frame size limits**: Control frames are capped at 64 KiB (`MAX_CONTROL_FRAME_SIZE`), data frames at 100 MiB (`MAX_FRAME_SIZE`), preventing unbounded memory allocation from malicious frames.
- **Clean EOF handling**: `recv_frame` properly distinguishes between EOF and errors.
- **Unknown frame types rejected**: `NotebookFrameType::try_from` returns an error for unknown type bytes.

### Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **Medium** | **No explicit socket permissions set.** The daemon creates the socket with `UnixListener::bind()` (line 501) but never sets permissions on the socket file itself. The socket inherits the process umask. On multi-user systems, if `~/.cache/runt/` is accessible to other users, the socket could be connectable. Defense: the parent directory is in `~/.cache/` which is typically `0700`, but this isn't explicitly enforced. | `daemon.rs:500-501` |
| **Low** | **No authentication on the IPC protocol.** Any process that can connect to the Unix socket can issue arbitrary commands (launch kernels, read/write blobs, sync notebooks). This is acceptable for a single-user desktop app where socket access implies local user access, but should be documented as a security boundary. | `daemon.rs:500-531` |
| **Info** | **Handshake deserialization correctly uses `recv_control_frame` (64 KiB limit).** The `route_connection` method (line 856) already applies the smaller frame limit for the initial handshake. No action needed. | `daemon.rs:856` |

### Recommendation

Verify the socket parent directory permissions are set to `0700` when creating it, or set socket file permissions to `0600` after binding.

---

## 3. Blob Store & HTTP Server Protocol

**Files**: `crates/runtimed/src/blob_store.rs`, `crates/runtimed/src/blob_server.rs`

### What it does

A content-addressed blob store persists notebook outputs (images, HTML) to disk, sharded by the first 2 characters of the SHA-256 hash. An HTTP server on `127.0.0.1` (random port) serves blobs for the webview to display.

### Strengths

- **Hash validation**: `validate_hash()` enforces exact 64-char hex strings, preventing path traversal attacks via the hash parameter.
- **Localhost-only binding**: `TcpListener::bind("127.0.0.1:0")` ensures the server is not network-accessible.
- **Content-addressed = no guessability**: 256-bit hashes are effectively unguessable, so the lack of authentication is acceptable.
- **Atomic writes**: Uses temp file + rename pattern to prevent partial reads.
- **Size limits**: 100 MiB max blob size prevents disk exhaustion from single blobs.
- **Immutable caching**: `Cache-Control: public, max-age=31536000, immutable` is correct for content-addressed resources.

### Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **Medium** | **`Access-Control-Allow-Origin: *` on all responses.** This allows any website in the user's browser to read blob data via fetch/XHR. While blobs are hashes and not guessable, if a hash leaks (e.g., in a shared notebook), a malicious website could exfiltrate the blob content. Consider restricting to the Tauri webview origin or using a nonce-based approach. | `blob_server.rs:103,119` |
| **Low** | **No rate limiting on the HTTP server.** A local process could flood the blob server with requests. Low risk since it requires local access. | `blob_server.rs:40-61` |
| **Info** | **`Content-Type` is stored from metadata and served without sanitization.** The `media_type` in `BlobMeta` is set by the caller (daemon output processing). If a malicious kernel output sets an unexpected content type (e.g., `text/html` with script), the blob server will serve it. The webview likely doesn't execute scripts from blob URLs, but this should be verified. | `blob_server.rs:96-100` |

---

## 4. Notebook Sync Protocol (Automerge CRDT)

**Files**: `crates/runtimed/src/notebook_sync_server.rs`, `crates/runtimed/src/connection.rs`

### What it does

Each open notebook creates a "room" in the daemon. Multiple peers (windows) exchange binary Automerge sync messages through typed frames. The daemon maintains the canonical document, persists it to disk, and broadcasts changes.

### Strengths

- **SHA-256 hashed persistence filenames**: `notebook_doc_filename()` hashes notebook IDs with SHA-256, preventing path traversal via malicious notebook_id values in the handshake.
- **Typed frame protocol**: Frame type byte prefix (0x00-0x03) provides clear message demarcation.
- **Automerge handles conflict resolution**: The CRDT model means out-of-order or duplicate sync messages are handled correctly by design.
- **Same Rust code in WASM and daemon**: Eliminates schema incompatibility between frontend and backend Automerge documents.

### Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **Low** | **`notebook_id` from handshake is not validated.** The `notebook_id` field in `Handshake::NotebookSync` is an arbitrary string from the client. While it's SHA-256 hashed before being used as a filename, it's used as a HashMap key directly. Extremely long notebook_ids could cause memory pressure, though this requires local socket access. | `connection.rs:48-62` |
| **Low** | **`working_dir` from handshake is used for project file detection.** The client-supplied `working_dir` is used to walk the filesystem looking for `pyproject.toml`, `pixi.toml`, etc. A malicious client could point this to sensitive directories. The walk-up stops at `.git` boundaries and home directory, which limits exposure. | `connection.rs:56-57` |

---

## 5. Kernel Launch Protocol

**Files**: `crates/runtimed/src/kernel_manager.rs`, `crates/kernel-launch/src/tools.rs`

### What it does

The daemon spawns Python (via ipykernel) or Deno kernel processes, passing a connection file containing ZMQ ports and an HMAC key for Jupyter wire protocol message signing.

### Strengths

- **Connection files use standard Jupyter protocol**: HMAC-SHA256 signing of wire protocol messages between kernel and client.
- **`kill_on_drop(true)`**: Kernel processes are killed when the manager is dropped, preventing orphaned processes.
- **Prewarmed environments are isolated**: Each pooled environment gets its own virtualenv.

### Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **Low** | **Kernel stdout/stderr are null-routed.** `cmd.stdout(Stdio::null()).stderr(Stdio::null())` means kernel process errors (crashes, import failures) are silently lost. This is primarily a debuggability concern, not a security issue. | `kernel_manager.rs:595-596` |
| **Info** | **Tool bootstrapping fetches binaries from conda-forge.** `get_deno_path()` and `get_uv_path()` use `rattler` to bootstrap tools from conda-forge. The channel integrity depends on conda-forge's package signing. This is standard practice but represents a supply chain trust boundary. | `kernel-launch/src/tools.rs` |

---

## 6. Daemon Singleton Protocol

**Files**: `crates/runtimed/src/singleton.rs`

### What it does

Uses `flock` (Unix) / `LockFileEx` (Windows) for file-based singleton enforcement. Writes daemon info (PID, socket path, blob port) to `daemon.json`.

### Strengths

- **Proper file locking**: Uses `LOCK_EX | LOCK_NB` for non-blocking exclusive lock.
- **Cleanup on drop**: `DaemonLock::drop()` removes the info file.
- **Lock file + info file separation**: The lock file provides mutual exclusion while the info file provides discovery.

### Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **Low** | **`daemon.json` is world-readable.** Written with `std::fs::write()` (line 183). Contains the blob server port number, which could be used by other local users to access the blob server. On single-user systems this is fine. | `singleton.rs:183` |
| **Info** | **Race between lock acquisition and info file read.** If a daemon crashes without cleaning up `daemon.json`, a new daemon will successfully acquire the lock but may briefly read stale info from the old file. The code handles this gracefully. | `singleton.rs:49-157` |

---

## Summary of Recommendations (Priority Order)

### Fixed in This Audit

1. **Trust key file permissions set to `0600` on Unix** (`runt-trust/src/lib.rs`). The HMAC key now has owner-only read/write permissions after creation.

2. **Unix socket permissions set to `0600`** (`daemon.rs`). The daemon socket is now restricted to the owning user after binding.

3. **Added `X-Content-Type-Options: nosniff` to blob server** (`blob_server.rs`). Prevents MIME sniffing attacks on served blobs.

### Already Correct

4. **Handshake uses `recv_control_frame` (64 KiB limit)** — already implemented at `daemon.rs:856`.

### Consider Fixing

5. **Restrict `Access-Control-Allow-Origin` on blob server** (`blob_server.rs:103,119`). Currently `*`, consider restricting to the Tauri webview origin to prevent cross-origin blob exfiltration. Deferred because the correct origin varies between dev and production modes.

6. **Document the security boundary**: local socket access = full daemon access. This is an intentional design choice for a desktop app but should be explicitly documented.

7. **Add canonicalization guarantee to trust signing** — either sort the serde_json::Map keys explicitly or use a dedicated canonical JSON library.

### Low Priority

8. Add key rotation support to the trust system.
9. Consider length limits on `notebook_id` in handshake.
10. Consider `Content-Security-Policy` headers on blob server responses to prevent script execution.
