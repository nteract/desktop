# Sidecar

A lightweight viewer for Jupyter outputs that runs alongside your terminal session. Built with a React frontend using [nteract elements](https://nteract-elements.vercel.app/) for rich output rendering and interactive widgets.

TODO: Include updated demo!

## Features

- **Rich Output Rendering** - text/plain, text/html, text/markdown, images, SVG, JSON tree view, ANSI terminal colors
- **Interactive Widgets** - Full ipywidgets support with two-way binding
- **anywidget Support** - Works with quak, drawdata, jupyter-scatter, and other anywidget-based libraries
- **Lightweight** - Single binary, no dependencies beyond your existing kernel

## Installation

Sidecar is not published to crates.io (it embeds UI assets at compile time that live outside the crate directory). Install via one of:

**Pre-built binaries** from [GitHub Releases](https://github.com/nteract/desktop/releases):

```bash
# Download the latest release for your platform
```

**Python package** (includes `runt` CLI with `runt sidecar` subcommand):

```bash
pip install runtimed
```

**From source** (requires building the UI first):

```bash
pnpm install && pnpm --dir apps/sidecar build
cargo build --release -p sidecar
```

## Usage

In a `jupyter console` session with Python, run:

```python
import subprocess
from ipykernel.connect import get_connection_file

connection_file = get_connection_file()

sidecar = subprocess.Popen(
    ["sidecar", "--quiet", connection_file],
    stdout=subprocess.PIPE, stderr=subprocess.PIPE
)
```

That will open a separate window showing the output of your Jupyter session.

### Interactive Widgets

Standard ipywidgets work out of the box:

```python
import ipywidgets as widgets
widgets.IntSlider(value=50, min=0, max=100, description='Test:')
```

anywidgets like `quak` for data exploration:

```python
%load_ext quak
import polars as pl
df = pl.read_parquet(
    "https://github.com/uwdata/mosaic/raw/main/data/athletes.parquet"
)
df
```

Or `drawdata` for interactive annotation:

```python
from drawdata import ScatterWidget
widget = ScatterWidget()
widget
```

## Development

The UI is built with Vite + React + Tailwind CSS. The built assets are embedded in the Rust binary at compile time.

### Building the UI

```bash
pnpm install
pnpm build
```

### Building sidecar

After building the UI:

```bash
cargo build -p sidecar
```

For debug builds with devtools access (Cmd+Option+I):

```bash
cargo build -p sidecar
RUST_LOG=info ./target/debug/sidecar /path/to/kernel.json
```

## nteract elements Integration

The sidecar UI uses components from [nteract elements](https://nteract-elements.vercel.app/), a shadcn/ui-based component registry for Jupyter interfaces.

### Adding Components

Components are added via the shadcn CLI from the nteract-elements registry:

```bash
cd ui
npx shadcn@latest add https://nteract-elements.vercel.app/r/<component>.json
```

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        Sidecar App                          │
├─────────────────────────────────────────────────────────────┤
│  ┌─────────────┐    ┌──────────────┐    ┌───────────────┐  │
│  │   Webview   │───▶│  Rust/Wry    │───▶│    Kernel     │  │
│  │  (React UI) │◀───│  (ZMQ/JSON)  │◀───│   (Python)    │  │
│  └─────────────┘    └──────────────┘    └───────────────┘  │
│        │                   │                    │          │
│   POST /message       Shell channel        comm_msg        │
│   (widget updates)    (to kernel)          (widget sync)   │
│                                                             │
│   globalThis.onMessage  IOPub channel     comm_open/msg    │
│   (receive outputs)     (from kernel)     (widget state)   │
└─────────────────────────────────────────────────────────────┘
```

## Related Resources

- [nteract elements docs](https://nteract-elements.vercel.app/docs)
- [anywidget AFM spec](https://anywidget.dev/en/afm/)
- [Jupyter Widget Protocol](https://jupyter-widgets.readthedocs.io/en/latest/examples/Widget%20Low%20Level.html)
