# dx

**Smart DataFrame display for Jupyter, built for [nteract](https://nteract.io).**

`dx` upgrades how pandas and polars DataFrames render in a notebook. Instead of serializing megabytes of HTML into your output cells, dx hands the data to nteract's content-addressed blob store and renders it through a fast Arrow/parquet grid. Your `.ipynb` stays tiny, the cell stays snappy, and AI agents reading the notebook get a compact per-column summary — dtypes, ranges, distinct/top values, null counts — instead of raw bytes.

## Install

```bash
# pandas
pip install "dx[pandas]"

# polars
pip install "dx[polars]"

# both
pip install "dx[pandas,polars]"
```

Python 3.10+.

## Use

```python
import dx
dx.install()

import pandas as pd
df = pd.read_parquet("large-dataset.parquet")
df  # rendered via nteract's sift grid — no base64 in your .ipynb
```

That's it. `dx.install()` is idempotent and automatically called by nteract's kernel bootstrap, so most nteract users never touch it directly. Calling it yourself is fine when you want the behavior in an environment nteract didn't configure for you (a standalone kernel, a test harness, etc.).

## What you get

- **Fast rendering.** Large DataFrames stream through the blob store; the `.ipynb` payload stays small.
- **AI-friendly summaries.** Every DataFrame ships a `text/llm+plain` column summary — dtypes, numeric ranges, string distinct/top values, null counts — so agents reason about the shape without materializing the whole table.
- **Visualization integration.** [Altair](https://altair-viz.github.io) and [Plotly](https://plotly.com/python/) are automatically switched to their nteract renderers for interactive output that works inside nteract's isolated iframe sandbox.
- **Narwhals-aware.** [narwhals](https://narwhals-dev.github.io/narwhals/)-wrapped DataFrames are unwrapped via `.to_native()` and dispatched through the pandas/polars path.
- **Safe outside nteract.** When no nteract runtime is reachable, `dx.install()` is a no-op. `import dx` is safe from plain Python, vanilla Jupyter, scripts, CI.

## Links

- Homepage: <https://nteract.io>
- Source & issues: <https://github.com/nteract/desktop>
- License: BSD-3-Clause
