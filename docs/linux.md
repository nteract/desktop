# Linux Install Options

nteract currently supports the AppImage from GitHub Releases as the Linux
desktop install and update artifact.

## Desktop Install

Download the AppImage from
[GitHub Releases](https://github.com/nteract/desktop/releases), make it
executable, and run it:

```bash
chmod +x nteract-stable-linux-x64.AppImage
./nteract-stable-linux-x64.AppImage
```

Nightly builds use the same shape:

```bash
chmod +x nteract-nightly-linux-x64.AppImage
./nteract-nightly-linux-x64.AppImage
```

The AppImage is the Linux artifact used by the desktop updater. It bundles the
desktop app, `runt`, `runtimed`, and `nteract-mcp` sidecars for that app
instance.

## Unsupported Package Formats

DEB, RPM, and APT repository installs are not currently supported.

`runtimed` is a user-local daemon. It owns per-user notebooks, kernels,
environment pools, settings sync, socket state, and MCP/CLI access. System
package managers are good at installing files under system-owned prefixes, but
they should not manage every user's daemon instance or run per-user daemon
repair from package maintainer scripts.

Until we design a first-class distro-native lifecycle, Linux releases do not
publish supported `.deb` or `.rpm` desktop packages and the release pipeline
does not publish new APT repository packages.

## Headless Runtime Installs

For headless Linux machines that need only the runtime stack, use the release
installer script with GitHub release artifacts from a checkout of this
repository:

```bash
./scripts/install-nightly-release --tag v2.3.5-nightly.YYYYMMDDHHMM
```

For source-built nightly installs on Linux development machines:

```bash
./scripts/install-nightly
```

Both scripts install channel-specific `runt`, `runtimed`, and `nteract-mcp`
binaries under `~/.local/share/` and configure a user-level systemd service.
This is a user-local runtime install, not a distro package-manager install.

## Troubleshooting

Check the daemon state:

```bash
runt daemon doctor --json
```

For nightly builds:

```bash
runt-nightly daemon doctor --json
```

Daemon logs live under `~/.cache/runt/` for stable and
`~/.cache/runt-nightly/` for nightly.
