# Audio Tracks — TTS for Notebooks via Blob Store

## Overview

Generate speech audio from notebook markdown cells using mlx-audio (Kokoro
on Apple Silicon), store the audio in the daemon's content-addressed blob
store, and create a track manifest that maps audio timestamps to Automerge
cell IDs. The blob store HTTP server already serves arbitrary MIME types
with CORS headers, so `<audio src="http://127.0.0.1:{port}/blob/{hash}">`
works out of the box.

## Why the blob store

The daemon already has a content-addressed blob store (`crates/runtimed/src/blob_store.rs`)
with an HTTP server (`crates/runtimed/src/blob_server.rs`). Key properties:

- `put(data, media_type) -> sha256_hash` — stores bytes atomically
- `GET /blob/{hash}` — serves with correct `Content-Type` from metadata sidecar
- `Cache-Control: immutable` + `Access-Control-Allow-Origin: *`
- Max 100 MiB per blob
- Content-addressed: same audio bytes = same hash = free dedup

Audio stored here persists across sessions and is instantly playable via URL.
Old clips stick around. Different voices or regenerations coexist naturally.

## Research Findings

### Blob store details

**File:** `crates/runtimed/src/blob_store.rs`

```rust
// Store audio
let hash = blob_store.put(&wav_bytes, "audio/wav").await?;
// Serve at: http://127.0.0.1:{blob_port}/blob/{hash}
// Content-Type: audio/wav (from metadata sidecar)
```

The HTTP server (`blob_server.rs`) sets `Content-Type` from `BlobMeta.media_type`.
No whitelist — `audio/wav`, `audio/mpeg`, `application/json` all work.

**Important:** The frontend's manifest resolution pipeline (`manifest-resolution.ts`)
calls `response.text()` which corrupts binary data. For audio, use the blob URL
directly as an `<audio src>` — don't go through the manifest pipeline.

**Write access:** The blob store is only writable from the daemon process
(no HTTP PUT endpoint). Audio must be written via daemon code or Python
bindings that call into the daemon.

### Python cell access

**File:** `crates/runtimed-py/src/session_core.rs`, `output.rs`

```python
cells: list[Cell] = await session.get_cells()
# or synchronous:
cells: list[Cell] = session.get_cells()

# Cell fields:
cell.id          # str — stable Automerge cell ID
cell.cell_type   # "code" | "markdown" | "raw"
cell.source      # str — cell content
cell.position    # str — fractional index hex for ordering
cell.outputs     # list[Output] — resolved outputs
cell.metadata    # dict — parsed from metadata_json
```

Cells are returned sorted by position. The `id` is stable across
reordering — perfect for cue references.

### mlx-audio Kokoro API

**Install:** `pip install mlx-audio` (or `uv pip install mlx-audio`)
**Model:** `mlx-community/Kokoro-82M-bf16` (~160MB, downloaded on first use)

```python
from mlx_audio.tts.utils import load_model

model = load_model("mlx-community/Kokoro-82M-bf16")
# model.sample_rate == 24000

for result in model.generate("Hello world", voice="af_heart", lang_code="a"):
    # result.audio: mx.array, shape (N,), dtype float32, mono PCM
    # result.samples: int (== audio.shape[0])
    # result.sample_rate: 24000
    # result.audio_duration: "HH:MM:SS.mmm"
    pass
```

`generate()` is a generator that yields one `GenerationResult` per text
segment (split on `\n+` by default). Each result contains:

| Field | Type | Description |
|-------|------|-------------|
| `audio` | `mx.array (N,)` | Float32 mono PCM waveform |
| `samples` | `int` | `audio.shape[0]` |
| `sample_rate` | `int` | 24000 |
| `segment_idx` | `int` | 0-based segment index |
| `audio_duration` | `str` | "HH:MM:SS.mmm" |
| `real_time_factor` | `float` | wall time / audio duration |

Available voices: `af_heart`, `af_bella`, `af_nova`, `af_sky`, `am_adam`,
`am_echo`, `bf_alice`, `bf_emma`, `bm_daniel`, `bm_george`, etc.

Language codes: `a` (American English), `b` (British), `j` (Japanese),
`z` (Mandarin), `e` (Spanish), `f` (French).

**Known issue:** On Python 3.13, `spacy-curated-transformers` may have an
ABI mismatch. Fix: `pip install --upgrade curated-tokenizers` or uninstall
`spacy-curated-transformers` if not needed.

## Data Model

### Track Manifest

Stored as a blob with `media_type: "application/json"`.

```json
{
  "version": 1,
  "audio_blob": "a1b2c3d4e5f6...",
  "format": "wav",
  "sample_rate": 24000,
  "voice": "af_heart",
  "model": "mlx-community/Kokoro-82M-bf16",
  "doc_heads": ["abc123..."],
  "created_at": "2026-03-16T...",
  "total_duration": 45.2,
  "cues": [
    {
      "start": 0.0,
      "end": 3.2,
      "cell_id": "f70c9751-...",
      "text": "Naming: Inkwell, Agents, and Identity"
    },
    {
      "start": 3.2,
      "end": 12.1,
      "cell_id": "f70c9751-...",
      "text": "There are three distinct things that need names..."
    }
  ]
}
```

**`doc_heads`** — Automerge document heads at generation time. Pins the
track to a document version. The text in cues makes the manifest a
self-contained artifact, but the Automerge history has the full version.

**`cell_id`** — stable Automerge cell IDs, not positions. Survives
cell reordering.

**`audio_blob`** — SHA-256 hash of the WAV in the blob store.

### Where tracks live in the Automerge doc

```
ROOT/metadata/
  audio_tracks/               ← Map
    {track_id}/
      manifest_blob: String   ← blob hash for the JSON manifest
      audio_blob: String      ← blob hash for the WAV (denormalized)
      voice: String
      created_at: String
```

Multiple tracks can coexist. Storing in doc metadata means other peers
see tracks appear via sync.

## Implementation Plan

### Phase 1: Python TTS module

**New file:** `python/runtimed/src/runtimed/tts.py`

A pure Python module that generates audio from cell data. No daemon
integration yet — just a function that takes cells and returns bytes + cues.

```python
def generate_track(
    cells: list,              # Cell objects from session.get_cells()
    voice: str = "af_heart",
    model_name: str = "mlx-community/Kokoro-82M-bf16",
    speed: float = 1.0,
    lang_code: str = "a",
) -> tuple[bytes, list[dict]]:
    """Generate a WAV audio track from notebook markdown cells.

    Returns (wav_bytes, cues) where cues is a list of
    {start, end, cell_id, text} dicts.
    """
```

Key implementation details:

- Filter to `cell_type == "markdown"` (skip code cells for v1)
- Split markdown into paragraphs for finer cue granularity
- Strip markdown formatting (headers, bold, links, tables) for cleaner speech
- Generate audio segment by segment, tracking cumulative offset
- Convert `mx.array` float32 → int16 PCM → WAV bytes via `wave` module
- Return raw WAV bytes (no file I/O) + cue list

Markdown stripping helper:
- `# Header` → "Header"
- `**bold**` → "bold"
- `[text](url)` → "text"
- `` `code` `` → "code"
- Pipe tables → skip entirely (they don't read well aloud)
- Code fences → skip

### Phase 2: MCP tool (quick integration)

Add to `python/nteract/src/nteract/_mcp_server.py`:

```python
@mcp.tool()
async def generate_audio_track(
    voice: str = "af_heart",
    notebook_id: str | None = None,
) -> str:
    """Generate a spoken audio narration of the notebook's markdown cells.

    Uses mlx-audio with the Kokoro TTS model (Apple Silicon).
    Returns a playable audio URL.
    """
    session = await _get_session(notebook_id)
    cells = session.get_cells()

    from runtimed.tts import generate_track
    wav_bytes, cues = generate_track(
        [{"id": c.id, "cell_type": c.cell_type, "source": c.source}
         for c in cells],
        voice=voice,
    )

    # Store audio in blob store
    # (need: a way to write to the blob store from Python)
    ...
```

**The blob store write problem:** The Python bindings don't currently
expose `blob_store.put()`. Options:

1. **Add `session.store_blob(data, media_type) -> hash`** to the Python
   bindings. This sends the data to the daemon which writes it to the blob
   store. Cleanest but requires Rust changes to `session_core.rs`.

2. **Write directly to the blob store directory.** The Python process
   knows the blob store path (from `SessionState.blob_store_path`). We
   could replicate the shard + hash logic in Python. Hacky but fast to
   prototype.

3. **Use the notebook kernel to emit display_data with audio/wav.**
   The kernel's output pipeline already goes through the blob store.
   But this conflates TTS with cell execution.

**Recommendation:** Option 1 (add `store_blob` to session). It's a small
Rust addition (~20 lines) and it's the right API for any future use of
the blob store from Python (not just audio).

### Phase 3: Daemon-native request

Add a new notebook request:

```
NotebookRequest::GenerateAudioTrack {
    voice: String,
    cell_ids: Option<Vec<String>>,  // None = all markdown cells
}
```

The daemon handler:
1. Reads cells from the Automerge doc
2. Calls into Python (via the kernel's environment or a separate process)
   to run `runtimed.tts.generate_track()`
3. Stores audio WAV + manifest JSON in the blob store
4. Writes track reference to `doc.metadata.audio_tracks`
5. Returns `AudioTrackGenerated { track_id, audio_url, manifest_url }`

This is the full integration — but Phase 2 (MCP tool) is sufficient for
the prototype.

### Phase 4: Frontend audio player (future)

- Audio player bar in the notebook chrome (above or below cells)
- Play/pause, seek, speed control
- Current cue highlights the corresponding cell
- Click a cell to seek to its cue
- Track list in notebook metadata panel

## File Changes Summary

### Phase 1 (Python TTS module)

| File | Change |
|------|--------|
| `python/runtimed/src/runtimed/tts.py` | New file — `generate_track()` |
| `python/runtimed/pyproject.toml` | Add `mlx-audio` as optional dep: `tts = ["mlx-audio>=0.4"]` |
| `python/runtimed/tests/test_tts.py` | Basic tests for markdown stripping and WAV encoding |

### Phase 2 (MCP tool + blob store write)

| File | Change |
|------|--------|
| `crates/runtimed-py/src/session_core.rs` | Add `store_blob(data, media_type) -> hash` |
| `python/nteract/src/nteract/_mcp_server.py` | Add `generate_audio_track` tool |
| `python/nteract/pyproject.toml` | Add `mlx-audio` as optional dep |

### Phase 3 (Daemon-native)

| File | Change |
|------|--------|
| `crates/notebook-protocol/src/protocol.rs` | Add `GenerateAudioTrack` request/response |
| `crates/runtimed/src/notebook_sync_server.rs` | Handle the request |
| `crates/notebook-doc/src/lib.rs` | Add `audio_tracks` map to schema |

## Playback Without Frontend Changes

For the prototype, audio is playable without any frontend work:

- **Via MCP:** The tool returns the blob URL. The agent can tell the user
  to open it, or use `afplay` on macOS: `afplay <(curl -s $BLOB_URL)`
- **Via browser:** Open `http://127.0.0.1:{blob_port}/blob/{audio_hash}`
  directly — the browser's native audio player handles WAV.
- **Via the app's devtools console:**
  ```javascript
  new Audio("http://127.0.0.1:65117/blob/abc123...").play()
  ```

## Open Questions

- **Code cells:** Skip for v1. Future: "code cell" placeholder, or LLM
  summary ("this cell trains a random forest classifier").

- **WAV vs MP3:** WAV is zero-dependency. MP3 needs ffmpeg but is ~10x
  smaller. Start with WAV. The blob store's 100 MiB limit means ~35 min
  of WAV at 24kHz/16-bit mono, which is plenty for most notebooks.

- **Model download:** Kokoro-82M-bf16 is ~160MB, downloaded on first use
  to `~/.cache/huggingface/`. Let it happen lazily — the MCP tool can
  report "downloading model..." on first call.

- **Incremental regeneration:** When cells change, regenerate only the
  affected cues and splice the audio. The manifest's `doc_heads` + per-cue
  `cell_id` make diffing possible. But this is a v2 feature.

- **Streaming:** Generate and play audio as it's produced (paragraph by
  paragraph) instead of waiting for the full track. Possible via chunked
  HTTP or WebSocket. v2 feature.

- **spacy ABI issue on Python 3.13:** May need to pin or work around.
  Test with the project's Python 3.12 venv first.