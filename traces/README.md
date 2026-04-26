# Trace Dumps

Drop local profiling traces, Safari timeline splats, notebooks, screenshots, and
other investigation artifacts here.

This directory is intentionally gitignored except for this README and its
`.gitignore`. Use `safari-timeline-splat` to unpack Safari Web Inspector exports:

```bash
uv run --package safari-timeline safari-timeline-splat path/to/recording.json traces/run-name
```

Project-backed notebooks opened from the repository root can also import the
parser directly:

```python
from pathlib import Path

from safari_timeline import SplatOptions, splat_recording

splat_recording(
    Path("path/to/recording.json"),
    Path("traces/run-name"),
    SplatOptions(screenshots="none"),
)
```
