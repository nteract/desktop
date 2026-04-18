# @nteract/dev-harness-electron

**Dev-only.** Never shipped. Never referenced by the production Tauri build.

Electron shell that mounts the same notebook frontend Tauri mounts, but opens the daemon's Unix socket directly from the main process so Playwright (on Chromium) can drive the UI headlessly. Exists because Tauri's WebKit runtime blocks WebDriver from stepping into sandboxed output iframes, which stopped us from reproducing the widget-sync stall end-to-end.

## Run it

Prereqs: repo-level `pnpm install`, direnv active, dev daemon running.

```bash
# Terminal 1 — daemon (or use `up` from nteract-dev MCP)
cargo xtask dev-daemon

# Terminal 2 — notebook frontend (Vite dev server on :5174)
pnpm --filter notebook-ui dev

# Terminal 3 — Electron harness
pnpm --filter @nteract/dev-harness-electron dev
```

The harness launches Electron, opens the dev daemon's Unix socket from the main process, and loads `http://localhost:5174` in a `BrowserWindow`. The renderer detects `window.electronAPI` and uses `ElectronTransport` instead of `TauriTransport`; everything else is the same frontend code.

## Security

- No network listener is added. The main process reads the existing `runtimed` Unix socket (mode 0600, user-scoped) via Node's `net.createConnection`.
- `BrowserWindow` runs with `contextIsolation: true`, `nodeIntegration: false`, `sandbox: true`. The preload's `contextBridge` surface is the only path between renderer JS and Node.
- This package is not compiled into any release artifact. `pnpm --filter notebook-ui build`, `cargo xtask build-app`, `cargo xtask install-nightly`, and the `.mcpb` packager all ignore it.

## Playwright

```bash
pnpm --filter @nteract/dev-harness-electron test:e2e
```

Tests live in `tests/`. They use Playwright's `_electron` API to launch the harness, then drive the renderer with full Chromium sandbox-iframe support.
