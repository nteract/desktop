# Releasing

How versioning, releases, and publishing work across the project.

## Version Scheme

The repo keeps a shared semver source version across its release inputs, but CI stamps desktop/CLI artifacts with channel-specific suffixes at publish time:

| Artifact | Where | Version source |
|---|---|---|
| nteract desktop app | GitHub Releases | `crates/notebook/tauri.conf.json` |
| `runt` CLI | GitHub Releases | `crates/runt/Cargo.toml` |
| `runtimed` daemon | Bundled in app + Python wheel | `crates/runtimed/Cargo.toml` |
| `runtimed` Python package | PyPI | `python/runtimed/pyproject.toml` |
| `nteract` Python package | PyPI | `python/nteract/pyproject.toml` |

Standard semver rules apply:

- **Major** — breaking changes to user-facing APIs or behavior
- **Minor** — new features, additive protocol/schema changes
- **Patch** — bug fixes

### Internal compatibility markers

Two independent version numbers handle compatibility, separate from the artifact version:

- **Protocol version** (`PROTOCOL_VERSION` in `crates/notebook-protocol/src/connection.rs`) — governs wire compatibility. Validated by the magic bytes preamble at connection time. Bump when the framing, handshake shape, or message serialization format changes.
- **Schema version** (`SCHEMA_VERSION` in `notebook-doc/src/lib.rs`) — governs Automerge document compatibility. Stored in the doc root. Bump when the document structure changes (v2 switched cells from an ordered list to a fractional-indexed map).

These are just incrementing integers. They evolve independently from each other and from the artifact version. A protocol bump doesn't force a major version bump — it depends on whether the change is user-facing.

## Bumping Versions

All six version sources must stay in sync. When preparing a release:

```bash
# Update all of these to the same version:
#   crates/runtimed/Cargo.toml
#   crates/runtimed-client/Cargo.toml
#   crates/runt/Cargo.toml
#   crates/notebook/Cargo.toml
#   crates/notebook/tauri.conf.json
#   python/runtimed/pyproject.toml
#   python/nteract/pyproject.toml

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
3. Builds desktop/CLI artifacts with a CI-stamped stable suffix (`-stable.{timestamp}`) while keeping stable Python packages at the plain `pyproject.toml` version
4. Publishes wheels to PyPI (stable release)
5. Creates a GitHub Release with all artifacts
6. Updates the `stable-latest` Tauri updater channel
7. Posts to Discord

The stable release publishes the Python packages to PyPI at the exact versions from `python/runtimed/pyproject.toml` and `python/nteract/pyproject.toml`. Desktop and CLI artifacts are stamped during the workflow, so the release tag is not the final desktop/CLI artifact version string.

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

This builds macOS and Linux Python artifacts for both `runtimed` and `nteract`, then publishes them to PyPI. Use this when you need to ship a Python patch without cutting a new desktop release.

## Tag Reference

| Tag pattern | Workflow | What it publishes |
|---|---|---|
| `v*` | `release-stable.yml` | Desktop app + CLI + Python (stable) |
| `python-v*` | `python-package.yml` | Python packages only (`runtimed` + `nteract`) |
| _(cron)_ | `release-nightly.yml` | Desktop app + CLI + Python (pre-release) |

## Protocol Version Changes

When making a breaking wire protocol change:

1. Bump `PROTOCOL_VERSION` in `crates/notebook-protocol/src/connection.rs`
2. Update `PROTOCOL_V2` string constant if the version string changes
3. Update `contributing/protocol.md`
4. Decide whether this warrants a major, minor, or patch version bump based on user impact

The magic bytes preamble rejects connections with mismatched protocol versions at the wire level, before any JSON parsing.

## Schema Version Changes

When changing the Automerge document structure:

1. Bump `SCHEMA_VERSION` in `crates/notebook-doc/src/lib.rs`
2. Add migration logic in the daemon's doc loading path (detect old schema, convert in-place)
3. Update the document schema comment in `notebook-doc/src/lib.rs`

Schema changes don't necessarily require a protocol bump — the wire format for sync frames stays the same, only the doc content changes.

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
- [ ] `PROTOCOL_VERSION` and `SCHEMA_VERSION` are correct for this release
- [ ] CI is green on `main`
- [ ] Changelog-worthy items use conventional commit prefixes (`feat`, `fix`, `perf`)
