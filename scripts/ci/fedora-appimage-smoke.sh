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

BIN_DIR="$WORKDIR/squashfs-root/usr/bin"

find_executable() {
  local name
  for name in "$@"; do
    if [[ -x "$BIN_DIR/$name" ]]; then
      printf '%s/%s\n' "$BIN_DIR" "$name"
      return 0
    fi
  done
  return 1
}

RUNT=$(find_executable runt-nightly runt) || true
RUNTIMED=$(find_executable runtimed-nightly runtimed) || true
MCP=$(find_executable nteract-mcp-nightly nteract-mcp) || true

for binary_name in runt runtimed nteract-mcp; do
  if ! find_executable "$binary_name-nightly" "$binary_name" >/dev/null; then
    echo "Expected executable missing from AppImage: $binary_name or $binary_name-nightly" >&2
    find "$BIN_DIR" -maxdepth 1 -type f -print >&2 || true
    exit 1
  fi
done

export HOME="$WORKDIR/home"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_DATA_HOME="$HOME/.local/share"
export XDG_CACHE_HOME="$HOME/.cache"
mkdir -p "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"

runt_version=$("$RUNT" --version)
runtimed_version=$("$RUNTIMED" --version)
echo "$runt_version"
echo "$runtimed_version"

RUNT_DIR=$(dirname "$RUNT")
export PATH="$RUNT_DIR:$PATH"

set +e
timeout 10s "$MCP" < /dev/null > nteract-mcp.stdout 2> nteract-mcp.stderr
mcp_status=$?
set -e

echo "=== nteract-mcp stdout ==="
cat nteract-mcp.stdout
echo
echo "=== nteract-mcp stderr ==="
cat nteract-mcp.stderr
echo

if [[ "$mcp_status" -ne 0 && "$mcp_status" -ne 124 ]]; then
  echo "nteract-mcp failed with status $mcp_status" >&2
  exit "$mcp_status"
fi

if grep -Eq "runt(-nightly)? not found" nteract-mcp.stderr; then
  echo "nteract-mcp could not find the bundled runt sidecar" >&2
  exit 1
fi

if ! grep -Eq "Validated runt(-nightly)? is available" nteract-mcp.stderr; then
  echo "nteract-mcp did not validate the bundled runt sidecar" >&2
  exit 1
fi

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

# shellcheck disable=SC2016
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

SERVICE_EXEC=$(grep -E '^ExecStart=' "$SERVICE_FILE" | head -n1 | sed 's/^ExecStart=//')
if [[ -z "$SERVICE_EXEC" ]]; then
  echo "service file has no ExecStart" >&2
  exit 1
fi

if [[ ! -x "$SERVICE_EXEC" ]]; then
  echo "daemon doctor did not copy runtimed to an executable durable path: $SERVICE_EXEC" >&2
  exit 1
fi

if ! cmp -s "$RUNTIMED" "$SERVICE_EXEC"; then
  echo "durable runtimed binary does not match the AppImage sidecar" >&2
  exit 1
fi

if ! grep -Fq "Environment=HOME=$HOME" "$SERVICE_FILE"; then
  echo "service file does not preserve HOME" >&2
  exit 1
fi
