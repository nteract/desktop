# LLM preview fields implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add optional `llm_preview` fields to `OutputManifest::Stream` and `OutputManifest::Error` so the RuntimeAgent writes a small, self-contained summary at spill time, and the MCP LLM path renders that summary instead of returning a bare blob URL.

**Architecture:** Preview is computed in `create_manifest` only when the ContentRef spills to blob (text/traceback ≥ 1 KiB threshold). The field lives on the manifest variant, is tagged `#[serde(default, skip_serializing_if = "Option::is_none")]` for forward/backward compat, and is consumed only by `resolve_output_for_llm` and `runt-mcp::structured`. Non-LLM paths (`.ipynb` save, frontend resolution) ignore it.

**Tech stack:** Rust (tokio, serde, automerge), PyO3 not touched, test harness uses `tokio::test` + existing `BlobStore`/`RuntimeStateDoc` helpers.

**Spec:** `docs/superpowers/specs/2026-04-15-llm-preview-fields-design.md`

---

## File map

| File | Role |
|------|------|
| `crates/runtimed/src/output_store.rs` | Add `StreamPreview`, `ErrorPreview` types; extend `OutputManifest::Stream` / `Error`; build previews in `create_manifest`; preview is pass-through in `resolve_manifest` (preview fields do not round-trip to `.ipynb`) |
| `crates/runtimed-client/src/output_resolver.rs` | In `resolve_output_for_llm`, use `llm_preview` instead of fetching the blob for Stream + Error when ContentRef is Blob. Synthesize rendered text with elision markers. |
| `crates/runt-mcp/src/structured.rs` | Emit `llm_preview` alongside blob URL in `manifest_output_to_structured` so MCP App widgets can show a summary without fetching the blob. |
| `crates/runtimed/tests/integration.rs` | End-to-end: write a long stream, fetch via the LLM-resolution path, assert head/tail/elision marker. |

No changes needed in `notebook-doc/src/runtime_state.rs` — `upsert_stream_output` takes an already-built manifest JSON value, so preview flows through transparently. Verified: the function only reads `text.blob` and `text.inline` to decide in-place vs append, never touches sibling fields.

---

## Task 1 — `StreamPreview` and `ErrorPreview` types

**Files:**
- Modify: `crates/runtimed/src/output_store.rs` (add types near `ContentRef`, around line 105)
- Test: `crates/runtimed/src/output_store.rs` (unit tests at bottom)

- [ ] **Step 1: Write failing unit tests for `StreamPreview::from_text`**

Add to `#[cfg(test)] mod tests` in `output_store.rs`:

```rust
#[test]
fn stream_preview_short_text_is_head_only() {
    let text = "line 1\nline 2\nline 3\n";
    let p = StreamPreview::from_text(text);
    assert_eq!(p.head, text);
    assert_eq!(p.tail, "");
    assert_eq!(p.total_bytes, text.len() as u64);
    assert_eq!(p.total_lines, 3);
}

#[test]
fn stream_preview_long_text_has_head_and_tail() {
    let text = (0..200)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let p = StreamPreview::from_text(&text);
    assert!(p.head.starts_with("line 0\n"));
    assert!(p.tail.ends_with("line 199"));
    // head ≤ 1 KiB, ≤ 40 lines
    assert!(p.head.len() <= 1024);
    assert!(p.head.lines().count() <= 40);
    assert!(p.tail.len() <= 1024);
    assert!(p.tail.lines().count() <= 40);
    assert_eq!(p.total_bytes, text.len() as u64);
    assert_eq!(p.total_lines, 200);
}

#[test]
fn stream_preview_caps_head_at_byte_limit_mid_line() {
    // A single very long line must still be capped at 1 KiB on a char boundary.
    let text = "x".repeat(10_000);
    let p = StreamPreview::from_text(&text);
    assert!(p.head.len() <= 1024);
    assert_eq!(p.total_bytes, 10_000);
}

#[test]
fn error_preview_keeps_last_frame() {
    let tb = serde_json::json!([
        "Traceback (most recent call last):",
        "  File \"<stdin>\", line 1",
        "ZeroDivisionError: division by zero",
    ]);
    let p = ErrorPreview::from_traceback_value(&tb);
    assert_eq!(p.last_frame, "ZeroDivisionError: division by zero");
    assert_eq!(p.frames, 3);
    assert!(p.total_bytes > 0);
}

#[test]
fn error_preview_strips_ansi_in_last_frame() {
    let tb = serde_json::json!([
        "Traceback…",
        "\x1b[31mValueError: bad input\x1b[0m",
    ]);
    let p = ErrorPreview::from_traceback_value(&tb);
    assert_eq!(p.last_frame, "ValueError: bad input");
}

#[test]
fn error_preview_empty_traceback() {
    let tb = serde_json::json!([]);
    let p = ErrorPreview::from_traceback_value(&tb);
    assert_eq!(p.last_frame, "");
    assert_eq!(p.frames, 0);
}
```

- [ ] **Step 2: Run tests — they should fail to compile (type not defined)**

Run: `cargo test -p runtimed --lib output_store::tests::stream_preview_short_text_is_head_only`
Expected: FAIL with "cannot find type `StreamPreview` in this scope"

- [ ] **Step 3: Implement `StreamPreview` and `ErrorPreview`**

Add to `output_store.rs` near the other manifest types (after `ContentRef` impl, around line 230):

```rust
/// Maximum head/tail size per side in bytes.
const PREVIEW_BYTE_CAP: usize = 1024;
/// Maximum head/tail size per side in lines.
const PREVIEW_LINE_CAP: usize = 40;

/// LLM-friendly summary of a spilled stream text blob. Populated at
/// manifest-creation time so readers never need to fetch the blob just
/// to describe it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamPreview {
    pub head: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tail: String,
    pub total_bytes: u64,
    pub total_lines: u64,
}

impl StreamPreview {
    pub fn from_text(text: &str) -> Self {
        let total_bytes = text.len() as u64;
        let total_lines = text.lines().count() as u64;
        let head = take_head(text, PREVIEW_LINE_CAP, PREVIEW_BYTE_CAP);
        // Tail is empty when head already covers the whole text (byte-wise).
        let tail = if head.len() as u64 >= total_bytes {
            String::new()
        } else {
            take_tail(text, PREVIEW_LINE_CAP, PREVIEW_BYTE_CAP)
        };
        Self {
            head,
            tail,
            total_bytes,
            total_lines,
        }
    }
}

/// LLM-friendly summary of a spilled traceback blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorPreview {
    pub last_frame: String,
    pub total_bytes: u64,
    pub frames: u32,
}

impl ErrorPreview {
    /// Build a preview from the raw traceback Value (array of strings).
    pub fn from_traceback_value(tb: &Value) -> Self {
        let frames_arr: Vec<&str> = tb
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let frames = frames_arr.len() as u32;
        let total_bytes = serde_json::to_string(tb)
            .map(|s| s.len() as u64)
            .unwrap_or(0);
        // Walk from the end, skipping empty frames.
        let raw_last = frames_arr
            .iter()
            .rev()
            .find(|s| !s.trim().is_empty())
            .copied()
            .unwrap_or("");
        let stripped = strip_ansi(raw_last);
        let last_frame = truncate_bytes(&stripped, PREVIEW_BYTE_CAP);
        Self {
            last_frame,
            total_bytes,
            frames,
        }
    }
}

fn take_head(text: &str, line_cap: usize, byte_cap: usize) -> String {
    let mut out = String::new();
    for (i, line) in text.split_inclusive('\n').enumerate() {
        if i >= line_cap {
            break;
        }
        if out.len() + line.len() > byte_cap {
            let remaining = byte_cap.saturating_sub(out.len());
            if remaining > 0 {
                out.push_str(&safe_byte_slice(line, 0, remaining));
            }
            break;
        }
        out.push_str(line);
    }
    // Handle the no-newline single-line-too-long case explicitly.
    if out.is_empty() && !text.is_empty() {
        out.push_str(&safe_byte_slice(text, 0, byte_cap));
    }
    out
}

fn take_tail(text: &str, line_cap: usize, byte_cap: usize) -> String {
    // Collect lines (preserving trailing newlines) and take the last N.
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let start = lines.len().saturating_sub(line_cap);
    let mut out = String::new();
    // Walk forward so the tail reads top-to-bottom.
    for line in &lines[start..] {
        if out.len() + line.len() > byte_cap {
            let remaining = byte_cap.saturating_sub(out.len());
            if remaining > 0 {
                // Keep the *last* `remaining` bytes for tail semantics.
                let start_byte = line.len() - remaining;
                out.push_str(&safe_byte_slice(line, start_byte, line.len()));
            }
            break;
        }
        out.push_str(line);
    }
    out
}

fn safe_byte_slice(s: &str, start: usize, end: usize) -> String {
    let mut lo = start.min(s.len());
    while lo > 0 && !s.is_char_boundary(lo) {
        lo -= 1;
    }
    let mut hi = end.min(s.len());
    while hi < s.len() && !s.is_char_boundary(hi) {
        hi += 1;
    }
    s[lo..hi].to_string()
}

fn truncate_bytes(s: &str, cap: usize) -> String {
    if s.len() <= cap {
        return s.to_string();
    }
    safe_byte_slice(s, 0, cap)
}

/// ANSI escape code stripper. Mirrors `runt-mcp::formatting::strip_ansi`
/// so the writer can normalize previews without depending on that crate.
fn strip_ansi(text: &str) -> String {
    use std::sync::LazyLock;
    static ANSI_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        #[allow(clippy::expect_used)]
        regex::Regex::new(r"\x1b\[[0-9;]*[A-Za-z]|\x1b\].*?\x07|\x1b\(B")
            .expect("valid ANSI regex")
    });
    ANSI_RE.replace_all(text, "").to_string()
}
```

Check whether `regex` is already a dependency of `runtimed`:

Run: `cargo tree -p runtimed -i regex 2>&1 | head -5`

If not listed, add to `crates/runtimed/Cargo.toml` under `[dependencies]`:

```toml
regex = "1"
```

- [ ] **Step 4: Run tests — should pass**

Run: `cargo test -p runtimed --lib output_store::tests::stream_preview_short_text_is_head_only output_store::tests::stream_preview_long_text_has_head_and_tail output_store::tests::stream_preview_caps_head_at_byte_limit_mid_line output_store::tests::error_preview_keeps_last_frame output_store::tests::error_preview_strips_ansi_in_last_frame output_store::tests::error_preview_empty_traceback`
Expected: 6 passed

- [ ] **Step 5: Commit**

```bash
git add crates/runtimed/src/output_store.rs crates/runtimed/Cargo.toml
git commit -m "feat(runtimed): add StreamPreview and ErrorPreview types"
```

---

## Task 2 — extend `OutputManifest` variants + `create_manifest`

**Files:**
- Modify: `crates/runtimed/src/output_store.rs` (enum variants, `create_manifest`, `resolve_manifest`)
- Test: `crates/runtimed/src/output_store.rs`

- [ ] **Step 1: Write failing tests for manifest shape**

Add to `#[cfg(test)] mod tests`:

```rust
#[tokio::test]
async fn small_stream_has_no_preview() {
    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::new(dir.path()).await.unwrap();
    let out = serde_json::json!({
        "output_type": "stream",
        "name": "stdout",
        "text": "hello\n",
    });
    let m = create_manifest(&out, &store, DEFAULT_INLINE_THRESHOLD).await.unwrap();
    let OutputManifest::Stream { text, llm_preview, .. } = m else {
        panic!("expected Stream");
    };
    assert!(matches!(text, ContentRef::Inline { .. }));
    assert!(llm_preview.is_none());
}

#[tokio::test]
async fn large_stream_has_preview() {
    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::new(dir.path()).await.unwrap();
    let big = (0..500).map(|i| format!("line {i}\n")).collect::<String>();
    let out = serde_json::json!({
        "output_type": "stream",
        "name": "stdout",
        "text": big.clone(),
    });
    let m = create_manifest(&out, &store, DEFAULT_INLINE_THRESHOLD).await.unwrap();
    let OutputManifest::Stream { text, llm_preview, .. } = m else {
        panic!("expected Stream");
    };
    assert!(matches!(text, ContentRef::Blob { .. }));
    let p = llm_preview.expect("preview when blob-stored");
    assert_eq!(p.total_lines, 500);
    assert_eq!(p.total_bytes, big.len() as u64);
    assert!(p.head.starts_with("line 0\n"));
    assert!(p.tail.trim_end().ends_with("line 499"));
}

#[tokio::test]
async fn small_error_has_no_preview() {
    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::new(dir.path()).await.unwrap();
    let out = serde_json::json!({
        "output_type": "error",
        "ename": "NameError",
        "evalue": "x",
        "traceback": ["frame 1", "frame 2"],
    });
    let m = create_manifest(&out, &store, DEFAULT_INLINE_THRESHOLD).await.unwrap();
    let OutputManifest::Error { traceback, llm_preview, .. } = m else {
        panic!("expected Error");
    };
    assert!(matches!(traceback, ContentRef::Inline { .. }));
    assert!(llm_preview.is_none());
}

#[tokio::test]
async fn large_error_has_preview_with_last_frame() {
    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::new(dir.path()).await.unwrap();
    let frames: Vec<String> = (0..200).map(|i| format!("frame line {i}")).collect();
    let out = serde_json::json!({
        "output_type": "error",
        "ename": "RecursionError",
        "evalue": "maximum recursion depth",
        "traceback": frames,
    });
    let m = create_manifest(&out, &store, DEFAULT_INLINE_THRESHOLD).await.unwrap();
    let OutputManifest::Error { traceback, llm_preview, .. } = m else {
        panic!("expected Error");
    };
    assert!(matches!(traceback, ContentRef::Blob { .. }));
    let p = llm_preview.expect("preview when blob-stored");
    assert_eq!(p.frames, 200);
    assert_eq!(p.last_frame, "frame line 199");
}

#[test]
fn manifest_without_preview_field_deserializes_to_none() {
    // Forwards-compat: old manifests without llm_preview must parse.
    let legacy = serde_json::json!({
        "output_type": "stream",
        "name": "stdout",
        "text": {"inline": "hello"},
    });
    let m: OutputManifest = serde_json::from_value(legacy).unwrap();
    let OutputManifest::Stream { llm_preview, .. } = m else {
        panic!("expected Stream");
    };
    assert!(llm_preview.is_none());
}
```

- [ ] **Step 2: Run tests — fail (struct fields don't exist yet)**

Run: `cargo test -p runtimed --lib output_store::tests::large_stream_has_preview`
Expected: compile error "struct `OutputManifest` has no variant `Stream` with named field `llm_preview`"

- [ ] **Step 3: Extend `OutputManifest` enum variants**

In `output_store.rs`, replace the `Stream` and `Error` variants in `OutputManifest` (around line 280):

```rust
    #[serde(rename = "stream")]
    Stream {
        name: String,
        text: ContentRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        llm_preview: Option<StreamPreview>,
    },
    #[serde(rename = "error")]
    Error {
        ename: String,
        evalue: String,
        traceback: ContentRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        llm_preview: Option<ErrorPreview>,
    },
```

- [ ] **Step 4: Update `create_manifest` stream + error branches**

Replace the `"stream"` arm in `create_manifest` (around line 347-361):

```rust
        "stream" => {
            let name = output
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("stdout")
                .to_string();
            let text_value = output
                .get("text")
                .cloned()
                .unwrap_or(Value::String(String::new()));
            let text_str = normalize_text(&text_value);
            let text =
                ContentRef::from_data(&text_str, "text/plain", blob_store, threshold).await?;
            let llm_preview = match &text {
                ContentRef::Blob { .. } => Some(StreamPreview::from_text(&text_str)),
                ContentRef::Inline { .. } => None,
            };
            OutputManifest::Stream { name, text, llm_preview }
        }
```

Replace the `"error"` arm (around line 362-387):

```rust
        "error" => {
            let ename = output
                .get("ename")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let evalue = output
                .get("evalue")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let traceback_value = output
                .get("traceback")
                .cloned()
                .unwrap_or(Value::Array(vec![]));
            let traceback_json = serde_json::to_string(&traceback_value)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let traceback =
                ContentRef::from_data(&traceback_json, "application/json", blob_store, threshold)
                    .await?;
            let llm_preview = match &traceback {
                ContentRef::Blob { .. } => Some(ErrorPreview::from_traceback_value(&traceback_value)),
                ContentRef::Inline { .. } => None,
            };
            OutputManifest::Error {
                ename,
                evalue,
                traceback,
                llm_preview,
            }
        }
```

- [ ] **Step 5: Fix `resolve_manifest` pattern matches**

In `resolve_manifest` (around line 627-649), update the match arms to ignore the new field:

```rust
        OutputManifest::Stream { name, text, .. } => {
            let resolved_text = text.resolve(blob_store).await?;
            Ok(serde_json::json!({
                "output_type": "stream",
                "name": name,
                "text": resolved_text,
            }))
        }
        OutputManifest::Error {
            ename,
            evalue,
            traceback,
            ..
        } => {
```

The `..` ignores `llm_preview` — `.ipynb` save path drops it intentionally.

- [ ] **Step 6: Fix every other `OutputManifest::Stream { .. }` / `Error { .. }` pattern in the crate**

Run: `cargo build -p runtimed 2>&1 | grep "pattern does not mention"`
Expected: lists every call site that destructures these variants without the new field.

Add `llm_preview: _` or `llm_preview: None` to each as appropriate. Likely call sites (grep ahead of time):

Run: `rg -n "OutputManifest::(Stream|Error)\s*\{" crates/ apps/`

For each hit, update pattern to ignore `llm_preview`. When constructing (e.g. tests), add `llm_preview: None`.

- [ ] **Step 7: Run all `output_store` tests — should pass**

Run: `cargo test -p runtimed --lib output_store::`
Expected: all green, including the 5 new tests.

- [ ] **Step 8: Run the full runtimed test suite — nothing else broken**

Run: `cargo test -p runtimed --lib`
Expected: all green.

- [ ] **Step 9: Commit**

```bash
git add crates/runtimed/src/output_store.rs
git commit -m "feat(runtimed): populate llm_preview on blob-spilled stream/error manifests"
```

---

## Task 3 — render preview in `resolve_output_for_llm`

**Files:**
- Modify: `crates/runtimed-client/src/output_resolver.rs` (Stream + Error branches of `resolve_output_for_llm`)
- Test: `crates/runtimed-client/src/output_resolver.rs` (tests module at bottom)

- [ ] **Step 1: Write failing tests**

Add to `#[cfg(test)] mod tests`:

```rust
#[tokio::test]
async fn llm_stream_with_blob_preview_renders_head_tail_marker() {
    let manifest = serde_json::json!({
        "output_type": "stream",
        "name": "stdout",
        "text": {"blob": "abc123", "size": 50000},
        "llm_preview": {
            "head": "line 0\nline 1\n",
            "tail": "line 98\nline 99\n",
            "total_bytes": 50000u64,
            "total_lines": 100u64,
        },
    });
    let out = resolve_output_for_llm(
        &manifest,
        &Some("http://localhost:9999".to_string()),
        &None,
        None,
    )
    .await
    .expect("output");
    let text = out.text.expect("stream text");
    assert!(text.starts_with("line 0\nline 1\n"));
    assert!(text.trim_end().ends_with("line 99"));
    assert!(text.contains("50000 bytes"));
    assert!(text.contains("http://localhost:9999/blob/abc123"));
    assert!(text.contains("elided") || text.contains("truncated"));
}

#[tokio::test]
async fn llm_stream_with_preview_no_base_url_still_renders() {
    let manifest = serde_json::json!({
        "output_type": "stream",
        "name": "stdout",
        "text": {"blob": "abc123", "size": 5000},
        "llm_preview": {
            "head": "head text\n",
            "tail": "tail text\n",
            "total_bytes": 5000u64,
            "total_lines": 10u64,
        },
    });
    let out = resolve_output_for_llm(&manifest, &None, &None, None)
        .await
        .expect("output");
    let text = out.text.expect("stream text");
    assert!(text.contains("head text"));
    assert!(text.contains("tail text"));
    assert!(!text.contains("http://"));
    assert!(text.contains("5000 bytes"));
}

#[tokio::test]
async fn llm_error_with_blob_preview_renders_last_frame() {
    let manifest = serde_json::json!({
        "output_type": "error",
        "ename": "RecursionError",
        "evalue": "oops",
        "traceback": {"blob": "tb_hash", "size": 8000},
        "llm_preview": {
            "last_frame": "RecursionError: maximum recursion depth",
            "total_bytes": 8000u64,
            "frames": 200u32,
        },
    });
    let out = resolve_output_for_llm(
        &manifest,
        &Some("http://localhost:9999".to_string()),
        &None,
        None,
    )
    .await
    .expect("output");
    assert_eq!(out.ename.as_deref(), Some("RecursionError"));
    assert_eq!(out.evalue.as_deref(), Some("oops"));
    let tb = out.traceback.expect("traceback");
    // First frame is the preserved last_frame; second is the elision marker.
    assert_eq!(tb[0], "RecursionError: maximum recursion depth");
    assert!(tb[1].contains("200"));
    assert!(tb[1].contains("http://localhost:9999/blob/tb_hash"));
}

#[tokio::test]
async fn llm_stream_without_preview_still_fetches_blob() {
    // Backwards compat: pre-change manifests have no llm_preview.
    // The resolver must fall back to reading the blob from disk.
    let dir = tempfile::tempdir().unwrap();
    let store_path = dir.path().to_path_buf();
    // Write blob to disk manually (bypassing BlobStore for simplicity)
    let hash = "abc1234567890def";
    let subdir = store_path.join(&hash[..2]);
    std::fs::create_dir_all(&subdir).unwrap();
    std::fs::write(subdir.join(&hash[2..]), "full stream text\n").unwrap();
    let manifest = serde_json::json!({
        "output_type": "stream",
        "name": "stdout",
        "text": {"blob": hash, "size": 18},
    });
    let out = resolve_output_for_llm(&manifest, &None, &Some(store_path), None)
        .await
        .expect("output");
    assert_eq!(out.text.as_deref(), Some("full stream text\n"));
}
```

- [ ] **Step 2: Run tests — expect failures**

Run: `cargo test -p runtimed-client --lib output_resolver::tests::llm_stream_with_blob_preview_renders_head_tail_marker`
Expected: FAIL (text doesn't contain expected markers — current code returns just the blob URL or drops the output).

- [ ] **Step 3: Update `resolve_output_for_llm` Stream + Error branches**

In `crates/runtimed-client/src/output_resolver.rs`, replace the `"stream"` and `"error"` arms of `resolve_output_for_llm` (around line 505-525):

```rust
        "stream" => {
            let name = manifest.get("name")?.as_str()?;
            let text_ref = manifest.get("text")?;
            // Fast path: if ContentRef is a Blob and we have a preview,
            // render the preview without fetching the blob.
            if let Some(blob_hash) = text_ref.get("blob").and_then(|v| v.as_str()) {
                if let Some(preview) = manifest.get("llm_preview") {
                    let text = render_stream_preview(preview, blob_hash, blob_base_url);
                    return Some(Output::stream(name, &text));
                }
            }
            let text = resolve_text_ref(text_ref, blob_base_url, blob_store_path).await?;
            Some(Output::stream(name, &text))
        }
        "error" => {
            let ename = manifest.get("ename")?.as_str()?.to_string();
            let evalue = manifest.get("evalue")?.as_str()?.to_string();
            let traceback_val = manifest.get("traceback")?;
            // Fast path: Blob traceback + preview → render without fetching.
            if let Some(blob_hash) = traceback_val.get("blob").and_then(|v| v.as_str()) {
                if let Some(preview) = manifest.get("llm_preview") {
                    let tb = render_error_preview(preview, blob_hash, blob_base_url);
                    return Some(Output::error(&ename, &evalue, tb));
                }
            }
            let traceback = if let Some(arr) = traceback_val.as_array() {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            } else {
                let tb_str =
                    resolve_text_ref(traceback_val, blob_base_url, blob_store_path).await?;
                serde_json::from_str::<Vec<String>>(&tb_str).ok()?
            };
            Some(Output::error(&ename, &evalue, traceback))
        }
```

Add the two render helpers near `resolve_text_ref` (around line 430):

```rust
/// Render a stream preview as a single text string for LLM consumption.
///
/// Shape:
///   <head>
///   … [{elided_lines} lines elided, {total_bytes} bytes total — full text at {url}] …
///   <tail>
///
/// When `tail` is empty (preview covered the whole text), drops the
/// elision marker and the tail section.
fn render_stream_preview(
    preview: &serde_json::Value,
    blob_hash: &str,
    blob_base_url: &Option<String>,
) -> String {
    let head = preview.get("head").and_then(|v| v.as_str()).unwrap_or("");
    let tail = preview.get("tail").and_then(|v| v.as_str()).unwrap_or("");
    let total_bytes = preview
        .get("total_bytes")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let total_lines = preview
        .get("total_lines")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    if tail.is_empty() {
        return head.to_string();
    }

    let head_lines = head.lines().count() as u64;
    let tail_lines = tail.lines().count() as u64;
    let elided_lines = total_lines.saturating_sub(head_lines + tail_lines);
    let url_clause = blob_base_url
        .as_ref()
        .map(|b| format!(" — full text at {}/blob/{}", b, blob_hash))
        .unwrap_or_default();

    let marker = format!(
        "… [{} lines elided, {} bytes total{}] …",
        elided_lines, total_bytes, url_clause
    );

    let mut out = String::with_capacity(head.len() + tail.len() + marker.len() + 2);
    out.push_str(head);
    if !head.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&marker);
    out.push('\n');
    out.push_str(tail);
    out
}

/// Render an error preview as a traceback array for `Output::error`.
/// First element is the preserved last frame; second is the elision marker.
fn render_error_preview(
    preview: &serde_json::Value,
    blob_hash: &str,
    blob_base_url: &Option<String>,
) -> Vec<String> {
    let last_frame = preview
        .get("last_frame")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let total_bytes = preview
        .get("total_bytes")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let frames = preview
        .get("frames")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let url_clause = blob_base_url
        .as_ref()
        .map(|b| format!(" — full traceback at {}/blob/{}", b, blob_hash))
        .unwrap_or_default();
    let marker = format!(
        "… [{} traceback frames, {} bytes total{}] …",
        frames, total_bytes, url_clause
    );
    vec![last_frame, marker]
}
```

- [ ] **Step 4: Run the new tests**

Run: `cargo test -p runtimed-client --lib output_resolver::tests::llm_stream_with_blob_preview_renders_head_tail_marker output_resolver::tests::llm_stream_with_preview_no_base_url_still_renders output_resolver::tests::llm_error_with_blob_preview_renders_last_frame output_resolver::tests::llm_stream_without_preview_still_fetches_blob`
Expected: 4 passed.

- [ ] **Step 5: Run full resolver test suite**

Run: `cargo test -p runtimed-client --lib output_resolver::`
Expected: all green — no regressions in existing resolver tests.

- [ ] **Step 6: Commit**

```bash
git add crates/runtimed-client/src/output_resolver.rs
git commit -m "feat(runtimed-client): render llm_preview for blob-spilled stream/error in LLM path"
```

---

## Task 4 — emit `llm_preview` in `runt-mcp` structured content

**Files:**
- Modify: `crates/runt-mcp/src/structured.rs` (stream + error branches)
- Test: `crates/runt-mcp/src/structured.rs`

- [ ] **Step 1: Write failing tests**

Add to the existing `#[cfg(test)] mod tests`:

```rust
#[test]
fn structured_stream_includes_preview_when_blob() {
    let manifest = json!({
        "output_type": "stream",
        "name": "stdout",
        "text": blob_ref("stream_hash", 50_000),
        "llm_preview": {
            "head": "line 0\n",
            "tail": "line 99\n",
            "total_bytes": 50_000u64,
            "total_lines": 100u64,
        },
    });
    let blob_base = Some("http://localhost:9999".to_string());
    let result = manifest_output_to_structured(&manifest, &blob_base);
    assert_eq!(result["text"], "http://localhost:9999/blob/stream_hash");
    assert_eq!(result["llm_preview"]["total_lines"], 100);
    assert_eq!(result["llm_preview"]["head"], "line 0\n");
}

#[test]
fn structured_error_includes_preview_when_blob() {
    let manifest = json!({
        "output_type": "error",
        "ename": "RecursionError",
        "evalue": "too deep",
        "traceback": blob_ref("tb_hash", 8_000),
        "llm_preview": {
            "last_frame": "RecursionError: too deep",
            "total_bytes": 8_000u64,
            "frames": 200u32,
        },
    });
    let blob_base = Some("http://localhost:9999".to_string());
    let result = manifest_output_to_structured(&manifest, &blob_base);
    assert_eq!(result["traceback"], "http://localhost:9999/blob/tb_hash");
    assert_eq!(result["llm_preview"]["frames"], 200);
    assert_eq!(result["llm_preview"]["last_frame"], "RecursionError: too deep");
}

#[test]
fn structured_stream_no_preview_for_inline() {
    let manifest = json!({
        "output_type": "stream",
        "name": "stdout",
        "text": inline_ref("hello"),
    });
    let result = manifest_output_to_structured(&manifest, &None);
    assert!(result.get("llm_preview").is_none());
}
```

- [ ] **Step 2: Run tests — expect failures**

Run: `cargo test -p runt-mcp --lib structured::tests::structured_stream_includes_preview_when_blob`
Expected: FAIL (no `llm_preview` key in result).

- [ ] **Step 3: Update `manifest_output_to_structured` stream + error arms**

In `crates/runt-mcp/src/structured.rs`, replace the `"stream"` arm (lines ~68-80):

```rust
        "stream" => {
            let name = manifest.get("name").cloned().unwrap_or(Value::Null);
            let text = manifest
                .get("text")
                .and_then(|cr| resolve_text_content_ref(cr, blob_base_url))
                .unwrap_or(Value::Null);
            let mut out = json!({
                "output_type": "stream",
                "name": name,
                "text": text,
            });
            if let Some(preview) = manifest.get("llm_preview") {
                out["llm_preview"] = preview.clone();
            }
            out
        }
```

Replace the `"error"` arm (lines ~81-111):

```rust
        "error" => {
            let traceback = manifest
                .get("traceback")
                .and_then(|cr| {
                    if let Some(inline) = cr.get("inline").and_then(|v| v.as_str()) {
                        serde_json::from_str::<Value>(inline).ok()
                    } else if let Some(hash) = cr.get("blob").and_then(|v| v.as_str()) {
                        blob_base_url
                            .as_ref()
                            .map(|base| Value::String(format!("{}/blob/{}", base, hash)))
                    } else if cr.is_array() {
                        Some(cr.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or(Value::Null);
            let mut out = json!({
                "output_type": "error",
                "ename": manifest.get("ename").cloned().unwrap_or(Value::Null),
                "evalue": manifest.get("evalue").cloned().unwrap_or(Value::Null),
                "traceback": traceback,
            });
            if let Some(preview) = manifest.get("llm_preview") {
                out["llm_preview"] = preview.clone();
            }
            out
        }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p runt-mcp --lib structured::`
Expected: all green including the 3 new tests.

- [ ] **Step 5: Commit**

```bash
git add crates/runt-mcp/src/structured.rs
git commit -m "feat(runt-mcp): pass through llm_preview in structured content for stream/error"
```

---

## Task 5 — integration test end-to-end

**Files:**
- Modify or create: `crates/runtimed/tests/integration.rs` (pick the right existing file — see Step 1)

- [ ] **Step 1: Identify the right integration test file**

Run: `ls crates/runtimed/tests/` and `rg -l "resolve_cell_outputs_for_llm|create_manifest.*blob_store" crates/runtimed/tests/ crates/runtimed-client/tests/ 2>&1`

Pick the file that already exercises the full IOPub→manifest→resolver chain. If none exists for this path, add to `crates/runtimed-client/tests/` or inline into `output_resolver.rs` as additional `#[tokio::test]`s.

- [ ] **Step 2: Write failing end-to-end test**

Template (adapt file path based on Step 1):

```rust
#[tokio::test]
async fn stream_blob_spill_is_renderable_by_llm_resolver() {
    use runtimed::blob_store::BlobStore;
    use runtimed::output_store::{create_manifest, DEFAULT_INLINE_THRESHOLD};
    use runtimed_client::output_resolver::resolve_cell_outputs_for_llm;

    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::new(dir.path()).await.unwrap();

    let big: String = (0..2_000)
        .map(|i| format!("stdout line {i}\n"))
        .collect();

    let raw = serde_json::json!({
        "output_type": "stream",
        "name": "stdout",
        "text": big.clone(),
    });

    let manifest = create_manifest(&raw, &store, DEFAULT_INLINE_THRESHOLD)
        .await
        .unwrap();
    let manifest_json = manifest.to_json();

    let outputs = resolve_cell_outputs_for_llm(
        &[manifest_json],
        &Some("http://127.0.0.1:1234".to_string()),
        &Some(dir.path().to_path_buf()), // local disk fallback is also available
        None,
    )
    .await;

    assert_eq!(outputs.len(), 1);
    let text = outputs[0].text.as_ref().expect("stream text");
    assert!(text.contains("stdout line 0"));
    assert!(text.contains("stdout line 1999"));
    assert!(text.contains("bytes total"));
}

#[tokio::test]
async fn error_blob_spill_is_renderable_by_llm_resolver() {
    use runtimed::blob_store::BlobStore;
    use runtimed::output_store::{create_manifest, DEFAULT_INLINE_THRESHOLD};
    use runtimed_client::output_resolver::resolve_cell_outputs_for_llm;

    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::new(dir.path()).await.unwrap();

    let frames: Vec<String> = (0..500).map(|i| format!("  frame {i} — file.py:{i}")).collect();

    let raw = serde_json::json!({
        "output_type": "error",
        "ename": "RecursionError",
        "evalue": "maximum recursion depth exceeded",
        "traceback": frames,
    });

    let manifest = create_manifest(&raw, &store, DEFAULT_INLINE_THRESHOLD)
        .await
        .unwrap();
    let manifest_json = manifest.to_json();

    let outputs = resolve_cell_outputs_for_llm(
        &[manifest_json],
        &Some("http://127.0.0.1:1234".to_string()),
        &Some(dir.path().to_path_buf()),
        None,
    )
    .await;

    assert_eq!(outputs.len(), 1);
    let out = &outputs[0];
    assert_eq!(out.ename.as_deref(), Some("RecursionError"));
    let tb = out.traceback.as_ref().expect("traceback");
    assert!(tb[0].contains("frame 499"));
    assert!(tb[1].contains("500"));
    assert!(tb[1].contains("traceback frames"));
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p runtimed --test integration -- stream_blob_spill_is_renderable_by_llm_resolver error_blob_spill_is_renderable_by_llm_resolver` (adjust `--test` flag to the file chosen in Step 1)
Expected: both passed.

- [ ] **Step 4: Commit**

```bash
git add crates/runtimed/tests/integration.rs
git commit -m "test(runtimed): e2e — blob-spilled stream and error are renderable via LLM resolver"
```

---

## Task 6 — lint, full-suite sanity, verify end-to-end

- [ ] **Step 1: Run lint/format**

Run: `cargo xtask lint --fix`
Expected: exit 0; no untracked reformat-only changes (if any, commit separately as `chore`).

- [ ] **Step 2: Full workspace test**

Run: `cargo test --workspace --lib`
Expected: all green.

- [ ] **Step 3: Verify via supervisor + live kernel**

Use the `up rebuild=true` supervisor tool (from the nteract-dev MCP server) to rebuild and restart the dev daemon with the new code.

Then, using nteract MCP tools against the dev daemon:

1. `create_notebook` → `launch_app` → `add_dependency` (none needed)
2. `create_cell` with source:
   ```python
   for i in range(3000):
       print(f"stream line {i}")
   ```
3. `execute_cell` — confirm the response comes back quickly (preview rendering, not blob fetch of 60 KB).
4. Inspect `get_cell` output — should contain `stream line 0`, `stream line 2999`, `bytes total`, and a `http://.../blob/...` URL.
5. Run a cell that raises:
   ```python
   def f(n):
       return f(n + 1)
   f(0)
   ```
6. Inspect outputs — error output should have `ename=RecursionError`, `evalue=...`, traceback containing the last frame + `… [N traceback frames, M bytes total — full traceback at http://...] …`.

- [ ] **Step 4: Run `sync-tool-cache` to double-check tool schemas unaffected**

Run: `cargo xtask sync-tool-cache`
Expected: `Done. Review the changes and commit.` with no diff (tool shapes didn't change, only manifest internals).

If there *is* a diff, something's leaked — investigate before committing.

- [ ] **Step 5: Commit any cleanup from lint-fix if needed**

```bash
git status
git add -p
git commit -m "chore: fmt"
```

- [ ] **Step 6: Push + open PR**

```bash
git push -u origin HEAD
gh pr create --title "feat(runtimed): llm_preview fields for blob-spilled stream and error outputs" --body "$(cat <<'EOF'
## Summary

- Adds `StreamPreview` and `ErrorPreview` types, populated in `create_manifest` when stream text or error traceback spills to the blob store.
- `resolve_output_for_llm` renders the preview (head/tail/last-frame + counts + blob URL) instead of fetching the blob, so LLM-facing text is informative without pulling megabytes into context.
- `runt-mcp` structured content carries `llm_preview` alongside the blob URL for widget consumption.

Addresses shakedown report issues #2 (traceback blob as bare string), #8 / #9 (stream blob leaking bare URL). Snapshot duplication (#1), execution_count staleness (#3), move_cell (#4), interrupt traceback (#5), timeout info (#6), get_cell separator (#7) tracked separately.

Spec: `docs/superpowers/specs/2026-04-15-llm-preview-fields-design.md`

## Test plan
- [ ] `cargo test -p runtimed --lib output_store::`
- [ ] `cargo test -p runtimed-client --lib output_resolver::`
- [ ] `cargo test -p runt-mcp --lib structured::`
- [ ] `cargo test -p runtimed --test integration -- stream_blob_spill error_blob_spill`
- [ ] Live dev daemon: 3000-line stream cell → response contains head, tail, bytes, blob URL
- [ ] Live dev daemon: deep recursion error → traceback has last frame + elision marker with URL
EOF
)"
```

---

## Self-review

**Spec coverage:**
- ✅ `StreamPreview` + `ErrorPreview` types — Task 1
- ✅ `OutputManifest::Stream` / `Error` extended with optional field — Task 2
- ✅ Populated in `create_manifest` on blob spill — Task 2
- ✅ Streaming append path: flows through `upsert_stream_output` untouched; each `create_manifest` call recomputes preview from the new full text — no extra work needed (called out in file map).
- ✅ `resolve_output_for_llm` uses preview instead of blob fetch — Task 3
- ✅ Backwards compat: missing preview field → `None`, resolver falls back to blob fetch — tests in Task 2 (deserialization) + Task 3 (resolver fallback)
- ✅ `runt-mcp` structured content emits preview — Task 4
- ✅ `.ipynb` save path drops preview intentionally — Task 2 Step 5 (pattern `..` in `resolve_manifest`)
- ✅ Integration test — Task 5
- ✅ Live verification — Task 6 Step 3

**Placeholder scan:** no TBDs, all code blocks complete.

**Type consistency:**
- `StreamPreview` fields (`head`, `tail`, `total_bytes`, `total_lines`) consistent across Task 1 (definition), Task 2 (creation), Task 3 (rendering), Task 4 (structured).
- `ErrorPreview` fields (`last_frame`, `total_bytes`, `frames`) consistent likewise.
- `Option<StreamPreview>` / `Option<ErrorPreview>` with `#[serde(default, skip_serializing_if = "Option::is_none")]` — forward/backward compat.
- `take_head` / `take_tail` / `truncate_bytes` / `safe_byte_slice` / `strip_ansi` all defined in Task 1; no later task references a helper not defined earlier.
