# Linux Install Options

nteract publishes Linux desktop builds through GitHub Releases and the APT
repository at `https://apt.runtimed.com`.

## Debian and Ubuntu

Use APT for Debian and Ubuntu systems. Native packages are the recommended
Linux install path because they integrate with the desktop and the per-user
`runtimed` systemd service.

### Stable

```bash
curl -fsSL https://apt.runtimed.com/nteract-keyring.gpg \
  | sudo gpg --dearmor --yes -o /usr/share/keyrings/nteract-keyring.gpg

echo "deb [arch=amd64 signed-by=/usr/share/keyrings/nteract-keyring.gpg] https://apt.runtimed.com stable main" \
  | sudo tee /etc/apt/sources.list.d/nteract.list

sudo apt update
sudo apt install nteract
```

### Nightly

Nightly builds install side-by-side with stable builds and use the
`nteract-nightly`, `runt-nightly`, and `runtimed-nightly` names.

```bash
curl -fsSL https://apt.runtimed.com/nteract-keyring.gpg \
  | sudo gpg --dearmor --yes -o /usr/share/keyrings/nteract-keyring.gpg

echo "deb [arch=amd64 signed-by=/usr/share/keyrings/nteract-keyring.gpg] https://apt.runtimed.com nightly main" \
  | sudo tee /etc/apt/sources.list.d/nteract-nightly.list

sudo apt update
sudo apt install nteract-nightly
```

You can enable both channels by keeping both source-list files.

## Direct Downloads

GitHub Releases include:

- `.deb` packages for direct Debian/Ubuntu installs when you do not want to add
  the APT repository.
- AppImage builds for desktop Linux systems outside the Debian/Ubuntu family.

For direct `.deb` installs:

```bash
sudo apt install ./nteract-stable-linux-x64.deb
```

For AppImage:

```bash
chmod +x nteract-stable-linux-x64.AppImage
./nteract-stable-linux-x64.AppImage
```

The AppImage is a portable fallback. It can run the app and bootstrap the
daemon, but persistent CLI installation from the temporary AppImage mount is
intentionally skipped. Use the APT or `.deb` package when you want the `runt`
CLI and system service to stay integrated across upgrades.

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
