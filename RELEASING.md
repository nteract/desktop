# Releasing

## Release Streams

| Stream | Tag | Trigger | Destination |
|--------|-----|---------|-------------|
| **Stable** | `v{version}-stable.{sha}` | Tag push (`v*`) or manual | GitHub Releases |
| **Nightly** | `v{version}-nightly.{sha}` | Cron (daily, 24h cadence) or manual | GitHub Pre-releases |
| **Python package** | `python-v{semver}` | Manual tag push | PyPI + GitHub Releases |

## Desktop App (nteract)

The desktop app, `runt` CLI, `runtimed` daemon, and `sidecar` are all built and released together via reusable workflow `.github/workflows/release-common.yml`, invoked by `.github/workflows/release-stable.yml` and `.github/workflows/release-nightly.yml`.

Stable releases run when a `v*` tag is pushed (or manually), and nightly pre-releases run every 24 hours. Both can also be triggered manually.

### Artifacts

| Platform | File |
|----------|------|
| macOS ARM64 (Apple Silicon) | `nteract-darwin-arm64.dmg` |
| macOS x64 (Intel) | `nteract-darwin-x64.dmg` |
| Windows x64 | `nteract-windows-x64.exe` |
| Linux x64 | `nteract-linux-x64.AppImage` |
| CLI (macOS ARM64) | `runt-darwin-arm64` |
| CLI (macOS x64) | `runt-darwin-x64` |
| CLI (Linux x64) | `runt-linux-x64` |

macOS builds are signed and notarized. Windows builds are not code signed.

### Crate publishing

`runt-cli` and `sidecar` are **not published to crates.io** (`publish = false`). Sidecar embeds UI assets from `apps/sidecar/dist/` via `rust-embed`, which requires files outside the crate directory.

## Python Package (runtimed)

The `runtimed` Python package provides bindings for the daemon and is released separately.

### 1. Bump the version

Edit `python/runtimed/pyproject.toml` and update the `version` field.

### 2. Create a PR

Open a PR with the version bump, get it reviewed and merged.

### 3. Tag and push

```
git tag python-v<version>
git push origin python-v<version>
```

The `python-package.yml` workflow triggers on `python-v*` tags and will:
- Build wheels for macOS (arm64 + x64)
- Publish to PyPI via trusted publishing (OIDC)
- Create a GitHub release with wheels and `runt` binaries

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
