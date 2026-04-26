# Safari Timeline

Utilities for turning Safari Web Inspector timeline exports into readable files.

The root workspace depends on this package, so project-backed notebooks opened
from the repository root can import it directly:

```python
from safari_timeline import SplatOptions, splat_recording
```

```bash
uv run --package safari-timeline safari-timeline-splat ~/Documents/127.0.0.1-recording.json .context/safari-recording
```

The splat output includes:

- `summary.json` with recording metadata, record counts, and frame timing stats.
- `records/all.jsonl` with one compact record per line.
- `records/<record-type>.jsonl` split by top-level Safari timeline record type.
- `screenshots/*.png` and `screenshots/index.jsonl` when screenshot extraction is enabled.

Use `--screenshots none` when you only need record metadata and frame timings.
