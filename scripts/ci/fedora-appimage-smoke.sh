#!/usr/bin/env bash
set -euo pipefail

APPIMAGE=${1:?usage: fedora-appimage-smoke.sh <path-to-AppImage>}

if [[ ! -f "$APPIMAGE" ]]; then
  echo "AppImage not found: $APPIMAGE" >&2
  exit 1
fi

WORKDIR=$(mktemp -d)
trap 'rm -rf "$WORKDIR"' EXIT

APPIMAGE_COPY="$WORKDIR/nteract.AppImage"
cp "$APPIMAGE" "$APPIMAGE_COPY"
chmod +x "$APPIMAGE_COPY"

cd "$WORKDIR"

echo "Extracting AppImage"
"$APPIMAGE_COPY" --appimage-extract > appimage-extract.log

RUNT="$WORKDIR/squashfs-root/usr/bin/runt"
RUNTIMED="$WORKDIR/squashfs-root/usr/bin/runtimed"
MCP="$WORKDIR/squashfs-root/usr/bin/nteract-mcp"

for binary in "$RUNT" "$RUNTIMED" "$MCP"; do
  if [[ ! -x "$binary" ]]; then
    echo "Expected executable missing from AppImage: $binary" >&2
    find "$WORKDIR/squashfs-root/usr/bin" -maxdepth 1 -type f -print >&2 || true
    exit 1
  fi
done

"$RUNT" --version
"$RUNTIMED" --version

export HOME="$WORKDIR/home"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_DATA_HOME="$HOME/.local/share"
export XDG_CACHE_HOME="$HOME/.cache"
mkdir -p "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"

# Simulate the environment AppRun gives child processes. The daemon doctor
# path should still call the host systemctl and should persist service files
# outside the temporary AppImage mount.
export APPDIR="$WORKDIR/squashfs-root"
export APPIMAGE="$APPIMAGE_COPY"
export ARGV0="$APPIMAGE_COPY"
export OWD="$WORKDIR"
export LD_LIBRARY_PATH="$APPDIR/usr/lib:$APPDIR/usr/lib/x86_64-linux-gnu"

set +e
"$RUNT" daemon doctor --fix --no-start --json > doctor.json 2> doctor.stderr
doctor_status=$?
set -e

echo "=== daemon doctor stdout ==="
cat doctor.json
echo
echo "=== daemon doctor stderr ==="
cat doctor.stderr
echo

if grep -Eiq 'symbol lookup error|error while loading shared libraries|version `[^`]+'\'' not found|liblzma|libsystemd' doctor.stderr doctor.json; then
  echo "daemon doctor appears to have used AppImage libraries for host systemctl" >&2
  exit 1
fi

if [[ "$doctor_status" -ne 0 ]]; then
  echo "daemon doctor failed with status $doctor_status" >&2
  exit "$doctor_status"
fi

SERVICE_FILE=$(find "$XDG_CONFIG_HOME/systemd/user" -maxdepth 1 -name 'runtimed*.service' -print -quit 2>/dev/null || true)
if [[ -z "$SERVICE_FILE" ]]; then
  echo "daemon doctor did not write a user systemd service file" >&2
  exit 1
fi

echo "=== user systemd service ==="
cat "$SERVICE_FILE"
echo

if grep -Fq "$WORKDIR/squashfs-root" "$SERVICE_FILE"; then
  echo "service file points into the temporary AppImage extraction" >&2
  exit 1
fi

if ! grep -Fq "ExecStart=$XDG_DATA_HOME" "$SERVICE_FILE"; then
  echo "service file does not point at the durable per-user data directory" >&2
  exit 1
fi

if ! grep -Fq "Environment=HOME=$HOME" "$SERVICE_FILE"; then
  echo "service file does not preserve HOME" >&2
  exit 1
fi
