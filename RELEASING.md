# Releasing

## Release Streams

| Stream | Tag | Trigger | Destination |
|--------|-----|---------|-------------|
| **Stable** | `v{version}-stable.{timestamp}` | Tag push (`v*`) or manual | GitHub Releases |
| **Nightly** | `v{version}-nightly.{timestamp}` | Cron (daily, 24h cadence) or manual | GitHub Pre-releases |
| **Python package** | `python-v{semver}` | Manual tag push | PyPI + GitHub Releases |

Timestamps are UTC in `YYYYMMDDHHMM` format, e.g. `v2.0.0-stable.202507010900`.

## Desktop App (nteract)

The desktop app, `runt` CLI, and `runtimed` daemon are all built and released together via reusable workflow `.github/workflows/release-common.yml`, invoked by `.github/workflows/release-stable.yml` and `.github/workflows/release-nightly.yml`.

Stable releases run when a `v*` tag is pushed (or manually), and nightly pre-releases run every 24 hours. Both can also be triggered manually.

> **Note:** Desktop releases also build Python wheels and attempt to publish the `runtimed` and `nteract` packages to PyPI via trusted publishing (`continue-on-error: true`, so publishing is best-effort and will silently skip if the version already exists). This means every stable and nightly release builds and attempts to push new Python wheels — the separate `python-v*` tag workflow is only needed for Python-specific patches that don't warrant a full desktop release.

### Artifacts

| Platform | File |
|----------|------|
| macOS ARM64 (Apple Silicon) | `nteract-{channel}-darwin-arm64.dmg` |
| Windows x64 | `nteract-{channel}-windows-x64.exe` |
| Linux x64 | `nteract-{channel}-linux-x64.AppImage` |
| Linux x64 | `nteract-{channel}-linux-x64.deb` |
| CLI (macOS ARM64) | `runt-darwin-arm64` |
| CLI (Linux x64) | `runt-linux-x64` |

macOS builds are signed and notarized. Windows builds are not code signed.

### Crate publishing

`runt-cli`, `runtimed-py`, `mcp-supervisor`, `runt-mcp`, and `xtask` are **not published to crates.io** (`publish = false`).

## Python Packages (runtimed, nteract)

The `runtimed` and `nteract` Python packages are released separately.

### 1. Bump the version

Edit `python/runtimed/pyproject.toml` and `python/nteract/pyproject.toml` and update the `version` field in each.

### 2. Create a PR

Open a PR with the version bump, get it reviewed and merged.

### 3. Tag and push

```
git tag python-v<version>
git push origin python-v<version>
```

The `python-package.yml` workflow triggers on `python-v*` tags and will:
- Build wheels for macOS arm64, macOS x86_64, and Linux x64
- Publish to PyPI via trusted publishing (OIDC)
- Create a GitHub release with wheels and nteract-dist packages

## Development

### Building from source

```bash
pnpm install
cargo xtask build
```

### Testing with local library changes

To test against unpublished runtimelib/jupyter-protocol changes, add to the root `Cargo.toml`:

```toml
[patch.crates-io]
runtimelib = { path = "../runtimed/crates/runtimelib" }
jupyter-protocol = { path = "../runtimed/crates/jupyter-protocol" }
```

## Migration from runt-notebook

If you have an older install from before the nteract rebrand:

```bash
# 1. Stop old daemon
launchctl bootout gui/$(id -u)/io.runtimed  # macOS
systemctl --user stop runtimed.service        # Linux

# 2. Remove old service config
rm ~/Library/LaunchAgents/io.runtimed.plist   # macOS

# 3. Remove old settings (optional — recreated with defaults)
rm -rf ~/Library/Application\ Support/runt-notebook  # macOS
rm -rf ~/.config/runt-notebook                        # Linux

# 4. Install nteract — registers the new daemon automatically
```
