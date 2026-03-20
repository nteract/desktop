# nteract

A fast, modern toolkit for Jupyter notebooks. Native desktop app with instant startup, realtime sync across windows and agents, and intelligent environment management.

Built on [runtimelib](https://crates.io/crates/runtimelib) and [jupyter-protocol](https://crates.io/crates/jupyter-protocol).

## Install

Download the latest release from [GitHub Releases](https://github.com/nteract/desktop/releases).

The desktop app bundles everything — `runt` CLI, `runtimed` daemon, and `sidecar`.

The Python bindings are available on PyPI:

```bash
pip install runtimed
```

## What's in here

| Component | Description |
|-----------|-------------|
| `nteract` | Desktop notebook editor (Tauri + React) |
| `runtimed` | Background daemon — environment pools, notebook sync, kernel execution |
| `runt` | CLI for managing kernels, notebooks, and the daemon |
| `sidecar` | Viewer for Jupyter kernel outputs |
| `runtimed` (PyPI) | Python bindings for the daemon |

## Usage

```bash
# Open a notebook
runt notebook path/to/notebook.ipynb

# Interactive console
runt console

# Daemon management
runt daemon status
runt daemon logs -f
```

List open notebooks with kernel and environment info:

```
$ runt notebooks
╭──────────────────────────────────────┬────────┬──────────────┬────────┬───────╮
│ NOTEBOOK                             │ KERNEL │ ENV          │ STATUS │ PEERS │
├──────────────────────────────────────┼────────┼──────────────┼────────┼───────┤
│ ~/notebooks/blobstore.ipynb          │ python │ uv:inline    │ idle   │ 1     │
│ d4c441d3-d862-4ab0-afe6-ff9145cc2f3d │ python │ uv:prewarmed │ idle   │ 1     │
╰──────────────────────────────────────┴────────┴──────────────┴────────┴───────╯
```

## Project structure

```
nteract/desktop
├── src/                    # Shared UI code (React components, hooks, utilities)
│   ├── components/
│   │   ├── ui/            # shadcn primitives (button, dialog, etc.)
│   │   ├── cell/          # Notebook cell components
│   │   ├── outputs/       # Output renderers (stream, error, display data)
│   │   ├── editor/        # CodeMirror editor
│   │   └── widgets/       # ipywidgets controls
│   └── lib/
│       └── utils.ts       # cn() and other utilities
├── apps/                   # App entry points
│   ├── notebook/          # Notebook Tauri frontend
│   └── sidecar/           # Sidecar WebView frontend
├── crates/                 # Rust code
│   ├── runt/              # CLI binary
│   ├── runtimed/          # Background daemon
│   ├── notebook/          # Notebook Tauri app
│   ├── sidecar/           # Sidecar wry/tao app
│   └── tauri-jupyter/     # Shared Tauri/Jupyter utilities
├── docs/                   # Architecture documentation
└── contributing/           # Developer guides
```

## Development

### Prerequisites

| Tool | Version | Install |
|------|---------|---------|
| Node.js | 20+ | https://nodejs.org |
| pnpm | 10.12+ | `corepack enable` |
| Rust | 1.90.0 | https://rustup.rs (version managed by `rust-toolchain.toml`) |

**Linux only:** Install GTK/WebKit dev libraries:
```bash
sudo apt-get install -y libgtk-3-dev libwebkit2gtk-4.1-dev libxdo-dev
```

### Quick start

```bash
pnpm install
cargo xtask build
```

### Development workflows

| Workflow | Command | Use when |
|----------|---------|----------|
| Hot reload | `cargo xtask dev` | Iterating on React UI |
| Standalone Vite | `cargo xtask vite` | Multi-window testing (Vite survives window closes) |
| Attach to Vite | `cargo xtask dev --attach` | Connect Tauri to already-running Vite |
| Debug build | `cargo xtask build` | Full debug build (frontend + rust) |
| Rust-only build | `cargo xtask build --rust-only` | Rebuild rust, reuse existing frontend |
| Run bundled | `cargo xtask run notebook.ipynb` | Run standalone binary |
| Release .app | `cargo xtask build-app` | Testing app bundle locally |
| Release DMG | `cargo xtask build-dmg` | Distribution (usually CI) |

### Build order

The UI must be built before Rust because:
- `crates/sidecar` embeds assets from `apps/sidecar/dist/` at compile time via [rust-embed](https://crates.io/crates/rust-embed)
- `crates/notebook` embeds assets from `apps/notebook/dist/` via Tauri

### Common commands

```bash
pnpm build                          # Build all UIs
cargo test                          # Run Rust tests
pnpm test:run                       # Run JS tests
cargo fmt                           # Format Rust
npx @biomejs/biome check --fix apps/notebook/src/ e2e/  # Format JS
cargo clippy --all-targets -- -D warnings               # Lint Rust
```

## Library crates

The underlying Rust libraries are published to crates.io:

- [`jupyter-protocol`](https://crates.io/crates/jupyter-protocol) — Jupyter messaging protocol
- [`runtimelib`](https://crates.io/crates/runtimelib) — Jupyter kernel interactions over ZeroMQ
- [`nbformat`](https://crates.io/crates/nbformat) — Notebook parsing

## License

BSD-3-Clause