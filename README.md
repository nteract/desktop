# nteract

A fast, modern toolkit for Jupyter notebooks. Native desktop app with instant startup, realtime sync across windows and agents, and intelligent environment management.

Built on [runtimelib](https://crates.io/crates/runtimelib) and [jupyter-protocol](https://crates.io/crates/jupyter-protocol).

## Install

Download the latest release from [GitHub Releases](https://github.com/nteract/desktop/releases).

The desktop app bundles everything — `runt` CLI and `runtimed` daemon.

The `runt` CLI and `runtimed` Python bindings ship with the app and stay up to date automatically. For nightly builds, use `runt-nightly` instead.

## What's in here

| Component | Description |
|-----------|-------------|
| `nteract` | Desktop notebook editor (Tauri + React) |
| `runtimed` | Background daemon — environment pools, notebook sync, kernel execution |
| `runt` | CLI for managing kernels, notebooks, and the daemon |
| `runtimed` (Python) | Python bindings for the daemon (ships with the app) |
## MCP Server

The nteract MCP server connects AI assistants to Jupyter notebooks through the daemon. Agents get 27 tools for working with notebooks — executing code, reading and writing cells, managing dependencies, and collaborating in real-time alongside humans in the desktop app.

The MCP server ships with the desktop app as `runt mcp`.

### Quick Start

#### Claude Code

```bash
# Stable
claude mcp add nteract -- runt mcp

# Nightly
claude mcp add nteract-nightly -- runt-nightly mcp
```

#### Manual JSON config

```json
{
  "mcpServers": {
    "nteract": {
      "command": "runt",
      "args": ["mcp"]
    }
  }
}
```

The `runt` CLI ships with the desktop app. Use `runt` (stable) or `runt-nightly` directly — no PyPI install needed.

## Usage

```bash
# Open a notebook
runt notebook path/to/notebook.ipynb

# Interactive console
runt jupyter console

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
│   ├── bindings/          # TypeScript types generated from Rust (ts-rs)
│   ├── components/
│   │   ├── ui/            # shadcn primitives (button, dialog, etc.)
│   │   ├── cell/          # Notebook cell components
│   │   ├── outputs/       # Output renderers (stream, error, display data)
│   │   ├── editor/        # CodeMirror editor
│   │   ├── isolated/      # Iframe security isolation (IsolatedFrame, CommBridgeManager)
│   │   └── widgets/       # ipywidgets controls
│   ├── hooks/             # Shared hooks (useSyncedSettings, useTheme)
│   ├── isolated-renderer/ # Code that runs inside isolated iframes
│   ├── lib/               # Shared utilities (cn(), dark-mode, error-boundary)
│   └── styles/            # Global stylesheets
├── apps/                   # App entry points
│   └── notebook/          # Notebook Tauri frontend
├── crates/                 # Rust code
│   ├── runt/              # CLI binary
│   ├── runtimed/          # Background daemon
│   ├── runtimed-py/       # Python bindings for the daemon
│   ├── runtimed-wasm/     # WASM Automerge bindings for frontend (same automerge crate as daemon)
│   ├── notebook/          # Notebook Tauri app
│   ├── notebook-doc/      # Shared Automerge document operations (cells, metadata, sync)
│   ├── notebook-protocol/ # Notebook wire protocol types
│   ├── notebook-sync/     # Notebook sync layer
│   ├── kernel-launch/     # Shared kernel launching API
│   ├── kernel-env/        # Environment progress reporting
│   ├── runt-mcp/          # Rust-native MCP server for notebook interaction
│   ├── runt-trust/        # HMAC trust verification
│   ├── runt-workspace/    # Workspace detection utilities
│   ├── runtimed-client/   # Shared client library for daemon communication
│   ├── repr-llm/          # LLM-friendly text summaries of visualization specs
│   ├── xtask/             # Build automation tasks
│   └── mcp-supervisor/    # nteract-dev MCP server (proxies runt mcp + adds dev tools)
├── python/                 # Python packages
│   ├── runtimed/          # PyPI: runtimed (Python bindings for daemon)
│   ├── nteract/           # PyPI: nteract (MCP server)
│   └── gremlin/           # Stress-testing agent for nteract notebooks (not published)
├── docs/                   # User-facing documentation
└── contributing/           # Developer guides
```

## Development

### Prerequisites

| Tool | Version | Install |
|------|---------|---------|
| Node.js | 20+ | https://nodejs.org |
| pnpm | 10.12+ | `corepack enable` |
| Rust | 1.94.0 | https://rustup.rs (version managed by `rust-toolchain.toml`) |

**Linux only:** Install GTK/WebKit dev libraries:
```bash
sudo apt-get install -y libgtk-3-dev libwebkit2gtk-4.1-dev libxdo-dev \
  libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

### Quick start

```bash
cargo xtask dev
```

### Development workflows

| Workflow | Command | Use when |
|----------|---------|----------|
| One-shot setup + dev | `cargo xtask dev` | First-time setup plus daemon + app in one command |
| Hot reload | `cargo xtask notebook` | Iterating on React UI |
| Standalone Vite | `cargo xtask vite` | Multi-window testing (Vite survives window closes) |
| Attach to Vite | `cargo xtask notebook --attach` | Connect Tauri to already-running Vite |
| Debug build | `cargo xtask build` | Full debug build (frontend + rust) |
| E2E debug build | `cargo xtask e2e build` | Debug build with built-in WebDriver server |
| Rust-only build | `cargo xtask build --rust-only` | Rebuild rust, reuse existing frontend |
| Run bundled | `cargo xtask run notebook.ipynb` | Run standalone binary |
| Lint (check) | `cargo xtask lint` | Check formatting and linting across Rust, JS/TS, Python |
| Lint (fix) | `cargo xtask lint --fix` | Auto-fix formatting and linting |
| Dev daemon | `cargo xtask dev-daemon` | Run per-worktree dev daemon |
| Install nightly (Linux/headless) | `cargo xtask install-nightly` | Build + install runtimed + runt + runt-proxy as the local nightly. Refuses on macOS and when an app bundle is installed. |
| Release .app | `cargo xtask build-app` | Testing app bundle locally |
| Release DMG | `cargo xtask build-dmg` | Distribution (usually CI) |
| Generate icons | `cargo xtask icons [source.png]` | Generate icon variants from source image |

`cargo xtask dev` runs the first-time bootstrap (`pnpm install` + `cargo xtask build`),
starts the per-worktree dev daemon, waits for it to be ready, and then launches the
notebook app. For repeat launches, use `cargo xtask dev --skip-install --skip-build`.

### Build order

The UI must be built before Rust because `crates/notebook` embeds assets from `apps/notebook/dist/` via Tauri.

### Common commands

```bash
pnpm build                          # Build notebook UI
cargo test                          # Run Rust tests
pnpm test:run                       # Run JS tests
cargo fmt                           # Format Rust
vp check --fix                      # Lint + format JS/TS
cargo clippy --all-targets -- -D warnings               # Lint Rust
```

## Library crates

The underlying Rust libraries are published to crates.io:

- [`jupyter-protocol`](https://crates.io/crates/jupyter-protocol) — Jupyter messaging protocol
- [`runtimelib`](https://crates.io/crates/runtimelib) — Jupyter kernel interactions over ZeroMQ
- [`nbformat`](https://crates.io/crates/nbformat) — Notebook parsing

## Contributing

See the `contributing/` directory for detailed guides:

- [Development workflow](contributing/development.md) — build commands, dev daemon, Zed tasks
- [Architecture](contributing/architecture.md) — runtime principles, daemon, state, sync
- [Protocol](contributing/protocol.md) — wire protocol, magic bytes preamble, version negotiation
- [Releasing](contributing/releasing.md) — versioning scheme, release procedures, tag conventions
- [Testing](contributing/testing.md) — Vitest, Rust, Python, E2E

## License

BSD-3-Clause