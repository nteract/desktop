# Releasing

How versioning, releases, and publishing work across the project.

## Version Scheme

All published artifacts share the same version and follow semver:

| Artifact | Where | Version source |
|---|---|---|
| nteract desktop app | GitHub Releases | `crates/notebook/tauri.conf.json` |
| `runt` CLI | GitHub Releases | `crates/runt/Cargo.toml` |
| `runtimed` daemon | Bundled in app + Python wheel | `crates/runtimed/Cargo.toml` |
| `runtimed` Python package | PyPI | `python/runtimed/pyproject.toml` |
| `sidecar` | Bundled in Python wheel | `crates/sidecar/Cargo.toml` |

**Major version = protocol version.** `PROTOCOL_VERSION` in `crates/runtimed/src/connection.rs` governs the major version. Any `runtimed 2.x.y` client can talk to any `2.x.y` daemon.

- **Major** — breaking wire protocol change (`PROTOCOL_VERSION` bump)
- **Minor** — new features (additive request/response/broadcast types)
- **Patch** — bug fixes, no protocol changes

## Bumping Versions

All five version sources must stay in sync. When preparing a release:

```bash
# Update all of these to the same version:
#   crates/runtimed/Cargo.toml
#   crates/runt/Cargo.toml
#   crates/notebook/Cargo.toml
#   crates/notebook/tauri.conf.json
#   crates/sidecar/Cargo.toml
#   python/runtimed/pyproject.toml

# Then let Cargo.lock catch up:
cargo check
```

Commit the version bump, then tag to trigger the release.

## Release Types

### Stable Release

Push a `v*` tag to `main`:

```bash
git tag v2.1.0
git push origin v2.1.0
```

This triggers `release-stable.yml` → `release-common.yml`, which:

1. Builds the desktop app (macOS, Windows, Linux)
2. Builds `runt` CLI binaries
3. Builds Python wheels at the version in `pyproject.toml` (no alpha stamp)
4. Publishes wheels to PyPI (stable release)
5. Creates a GitHub Release with all artifacts
6. Updates the `stable-latest` Tauri updater channel
7. Posts to Discord

The stable release publishes the Python package to PyPI at the exact version from `pyproject.toml`. This means tagging `v2.1.0` also ships `runtimed==2.1.0` on PyPI — no separate Python tag needed.

### Nightly Release

Runs automatically at 9am UTC daily via `release-nightly.yml`, or manually via workflow dispatch.

Same pipeline as stable, but:

- Desktop version gets a `-nightly.{timestamp}` suffix
- Python wheels get an alpha stamp: `2.0.1a202507150900` (PEP 440)
- App is branded "nteract Nightly" with a separate bundle ID (side-by-side install)
- GitHub Release is marked as prerelease
- CLI binary is named `runt-nightly`

Nightly Python wheels are installable with:

```bash
pip install runtimed --pre
```

### Python-Only Release

For Python-specific fixes that don't need a full desktop release, use the dedicated `python-package.yml` workflow:

```bash
# Bump python/runtimed/pyproject.toml (and Cargo.tomls if Rust changed)
git tag python-v2.1.1
git push origin python-v2.1.1
```

This builds macOS + Linux wheels and publishes to PyPI. Use this when you need to ship a Python patch without cutting a new desktop release.

## Tag Reference

| Tag pattern | Workflow | What it publishes |
|---|---|---|
| `v*` | `release-stable.yml` | Desktop app + CLI + Python (stable) |
| `python-v*` | `python-package.yml` | Python wheels only |
| _(cron)_ | `release-nightly.yml` | Desktop app + CLI + Python (pre-release) |

## Protocol Version Changes

When making a breaking wire protocol change:

1. Bump `PROTOCOL_VERSION` in `crates/runtimed/src/connection.rs`
2. Bump the major version in all six version sources
3. Update `PROTOCOL_V2` string constant if the version string changes
4. Update `contributing/protocol.md` versioning table
5. Tag as the new major version (e.g., `v3.0.0`)

The notebook sync path hard-fails on protocol mismatch. The pool IPC path sends `protocol_version` in the handshake for forward compatibility.

See `contributing/protocol.md` for the full versioning contract.

## CI Internals

The reusable `release-common.yml` accepts inputs from the nightly/stable callers:

- `github_release_prerelease: true` → applies PEP 440 alpha stamp to Python version
- `github_release_prerelease: false` → uses `pyproject.toml` version as-is

Python wheels are always built (macOS arm64, Linux x64, Windows x64) and always published. `continue-on-error: true` on the PyPI step handles duplicate version conflicts (e.g., re-running a workflow).

Desktop version is computed as `{runt-cli version}-{suffix}.{timestamp}` where suffix is `nightly` or `stable`. This is stamped into `tauri.conf.json` and `Cargo.toml` at build time — not committed.

### Trusted Publishing

PyPI publishing uses OIDC trusted publishing (no API tokens). The GitHub Actions workflow identity is registered as a trusted publisher on PyPI for the `runtimed` package. Both `release-common.yml` and `python-package.yml` use this.

## Checklist

Before tagging a stable release:

- [ ] All version sources bumped and in sync
- [ ] `cargo check` passes (Cargo.lock updated)
- [ ] `PROTOCOL_VERSION` matches major version
- [ ] CI is green on `main`
- [ ] Changelog-worthy items use conventional commit prefixes (`feat`, `fix`, `perf`)