# dx

Efficient display and blob-store uploads for Python kernels running under nteract.

`dx` lets a Python kernel push bytes directly to the nteract daemon's blob store via a dedicated Jupyter comm, bypassing the IOPub "raw bytes" anti-pattern. Display bundles carry a tiny reference MIME (`application/vnd.nteract.blob-ref+json`) instead of megabytes of parquet/image/video data.

## Usage

```python
import dx
dx.install()

import pandas as pd
df = pd.read_parquet("big.parquet")
df  # rendered via the sift parquet renderer from a blob reference
```

Low-level:

```python
ref = dx.put(open("image.png", "rb").read(), content_type="image/png")
dx.display_blob_ref(ref, content_type="image/png")
```

See `docs/superpowers/specs/2026-04-13-nteract-dx-design.md` for the protocol.

## In vanilla Jupyter or plain `python`

`dx.install()` is a no-op when no nteract runtime agent is reachable. `dx.display(df)` falls back to raw-bytes display, and `dx.put(...)` raises `DxNoAgentError`. The library is safe to import anywhere.
