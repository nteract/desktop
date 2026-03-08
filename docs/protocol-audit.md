# Protocol Security Audit

**Date**: 2026-03-07 (updated 2026-03-08)
**Scope**: All IPC, sync, trust, and network protocols in the nteract/desktop codebase

## Executive Summary

The codebase implements six major protocol surfaces: daemon IPC (Unix sockets / named pipes), Automerge notebook sync, HMAC-SHA256 dependency trust, a localhost blob HTTP server, Jupyter kernel wire protocol, and a singleton lock mechanism. Overall the protocol design is solid, with several good security practices already in place (constant-time HMAC comparison, content-addressed blob hashing with strict validation, SHA-256 hashed persistence filenames).

The initial audit identified three medium-severity issues, all of which have been fixed:
- Trust key file permissions hardened to `0600`
- Unix socket permissions hardened to `0600`
- `X-Content-Type-Options: nosniff` added to blob server

This document now tracks only remaining open items.

---

## 1. Trust Protocol (HMAC-SHA256 Dependency Signing)

**Files**: `crates/runt-trust/src/lib.rs`

### Strengths

- **Constant-time comparison**: Uses `mac.verify_slice()`, preventing timing attacks.
- **Signs only dependency metadata**: Cell edits don't invalidate trust, reducing user friction while maintaining supply-chain protection.
- **Per-machine keys**: Keys never leave the machine, preventing cross-machine signature forgery.
- **Typosquat detection**: Separate module (`crates/notebook/src/typosquat.rs`) warns about packages similar to popular ones.
- **Trust key file permissions**: Set to `0600` on Unix after creation.

### Open Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **Low** | **No key rotation mechanism.** If the key is compromised, all previously signed notebooks remain trusted. A key rotation feature (re-sign all notebooks with new key) would limit blast radius. | `runt-trust/src/lib.rs:69-97` |
| **Low** | **`extract_signable_content` uses `serde_json::to_string` for canonicalization.** `serde_json` does not guarantee key ordering for `serde_json::Map` (it preserves insertion order). In practice this works because the function constructs a fresh `serde_json::Map` with `insert()` in a fixed order, but this is fragile. | `runt-trust/src/lib.rs:106-121` |
| **Info** | **`RUNT_TRUST_KEY_PATH` env override could be used in production.** The environment variable is intended for testing but is unconditionally checked. Requires local code execution, so low risk. | `runt-trust/src/lib.rs:59` |

---

## 2. Daemon IPC Protocol (Unix Socket / Named Pipe)

**Files**: `crates/runtimed/src/connection.rs`, `crates/runtimed/src/daemon.rs`

### Strengths

- **Unix sockets have filesystem-level access control**: Only the owning user can connect.
- **Socket permissions explicitly set to `0600`** after binding.
- **Frame size limits**: Control frames capped at 64 KiB, data frames at 100 MiB.
- **Clean EOF handling**: `recv_frame` properly distinguishes between EOF and errors.
- **Unknown frame types rejected**: `NotebookFrameType::try_from` returns an error for unknown type bytes.
- **Handshake timeout**: 10-second timeout on initial handshake read prevents stalled connections.

### Open Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **Low** | **No authentication on the IPC protocol.** Any process that can connect to the Unix socket can issue arbitrary commands. Acceptable for a single-user desktop app where socket access implies local user access, but should be documented as a security boundary. | `daemon.rs:500-531` |

---

## 3. Blob Store & HTTP Server Protocol

**Files**: `crates/runtimed/src/blob_store.rs`, `crates/runtimed/src/blob_server.rs`

### Strengths

- **Hash validation**: `validate_hash()` enforces exact 64-char hex strings, preventing path traversal.
- **Localhost-only binding**: `TcpListener::bind("127.0.0.1:0")` ensures not network-accessible.
- **Content-addressed = no guessability**: 256-bit hashes are effectively unguessable.
- **Atomic writes**: Uses temp file + rename pattern to prevent partial reads.
- **Size limits**: 100 MiB max blob size.
- **Immutable caching**: Correct `Cache-Control` for content-addressed resources.
- **MIME sniffing prevention**: `X-Content-Type-Options: nosniff` on all responses.

### Open Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **Medium** | **`Access-Control-Allow-Origin: *` on all responses.** Allows any website in the user's browser to read blob data via fetch/XHR. If a hash leaks (e.g., in a shared notebook), a malicious website could exfiltrate the blob content. Consider restricting to the Tauri webview origin. Deferred because the correct origin varies between dev and production modes. | `blob_server.rs:103,119` |
| **Low** | **No rate limiting on the HTTP server.** A local process could flood the blob server. Low risk since it requires local access. | `blob_server.rs:40-61` |
| **Info** | **`Content-Type` is stored from metadata and served without sanitization.** If a malicious kernel output sets an unexpected content type (e.g., `text/html` with script), the blob server will serve it. The webview likely doesn't execute scripts from blob URLs, but this should be verified. | `blob_server.rs:96-100` |

---

## 4. Notebook Sync Protocol (Automerge CRDT)

**Files**: `crates/runtimed/src/notebook_sync_server.rs`, `crates/runtimed/src/connection.rs`

### Strengths

- **SHA-256 hashed persistence filenames**: Prevents path traversal via malicious notebook_id values.
- **Typed frame protocol**: Frame type byte prefix provides clear message demarcation.
- **Automerge handles conflict resolution**: The CRDT model handles out-of-order or duplicate sync messages by design.
- **Same Rust code in WASM and daemon**: Eliminates schema incompatibility.

### Open Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **Low** | **`notebook_id` from handshake is not validated.** Arbitrary string used as HashMap key directly. Extremely long notebook_ids could cause memory pressure, though this requires local socket access. | `connection.rs:48-62` |
| **Low** | **`working_dir` from handshake is used for project file detection.** A malicious client could point this to sensitive directories. The walk-up stops at `.git` boundaries and home directory, which limits exposure. | `connection.rs:56-57` |

---

## 5. Kernel Launch Protocol

**Files**: `crates/runtimed/src/kernel_manager.rs`, `crates/kernel-launch/src/tools.rs`

### Strengths

- **Connection files use standard Jupyter protocol**: HMAC-SHA256 signing of wire protocol messages.
- **`kill_on_drop(true)`**: Kernel processes are killed when the manager is dropped.
- **Prewarmed environments are isolated**: Each pooled environment gets its own virtualenv.

### Open Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **Low** | **Kernel stdout/stderr are null-routed.** Kernel process errors (crashes, import failures) are silently lost. Primarily a debuggability concern. | `kernel_manager.rs:595-596` |
| **Info** | **Tool bootstrapping fetches binaries from conda-forge.** Channel integrity depends on conda-forge's package signing. Standard practice but represents a supply chain trust boundary. | `kernel-launch/src/tools.rs` |

---

## 6. Daemon Singleton Protocol

**Files**: `crates/runtimed/src/singleton.rs`

### Strengths

- **Proper file locking**: Uses `LOCK_EX | LOCK_NB` for non-blocking exclusive lock.
- **Cleanup on drop**: `DaemonLock::drop()` removes the info file.
- **Lock file + info file separation**: Mutual exclusion + discovery.

### Open Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **Low** | **`daemon.json` is world-readable.** Contains the blob server port number, which could be used by other local users to access the blob server. Fine on single-user systems. | `singleton.rs:183` |
| **Info** | **Race between lock acquisition and info file read.** If a daemon crashes without cleanup, a new daemon may briefly read stale info. The code handles this gracefully. | `singleton.rs:49-157` |

---

## Open Recommendations (Priority Order)

1. **Restrict `Access-Control-Allow-Origin` on blob server** (`blob_server.rs:103,119`). Currently `*`, consider restricting to the Tauri webview origin to prevent cross-origin blob exfiltration. Deferred because the correct origin varies between dev and production modes.

2. **Document the security boundary**: local socket access = full daemon access. This is an intentional design choice for a desktop app but should be explicitly documented.

3. **Add canonicalization guarantee to trust signing** — either sort the serde_json::Map keys explicitly or use a dedicated canonical JSON library.

4. Add key rotation support to the trust system.

5. Consider length limits on `notebook_id` in handshake.

6. Consider `Content-Security-Policy` headers on blob server responses to prevent script execution.
