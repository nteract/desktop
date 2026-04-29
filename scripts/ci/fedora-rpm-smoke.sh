#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: fedora-rpm-smoke.sh <path-to-rpm> <stable|nightly> [expected-version]" >&2
}

RPM_PATH=${1:-}
CHANNEL=${2:-}
EXPECTED_VERSION=${3:-}

if [[ -z "$RPM_PATH" || -z "$CHANNEL" ]]; then
  usage
  exit 2
fi

case "$CHANNEL" in
  stable)
    PACKAGE_NAME="nteract"
    CLI_NAME="runt"
    DAEMON_NAME="runtimed"
    MCP_NAME="nteract-mcp"
    APP_NAME="nteract"
    ;;
  nightly)
    PACKAGE_NAME="nteract-nightly"
    CLI_NAME="runt-nightly"
    DAEMON_NAME="runtimed-nightly"
    MCP_NAME="nteract-mcp-nightly"
    APP_NAME="nteract-nightly"
    ;;
  *)
    echo "Unsupported channel: $CHANNEL" >&2
    exit 2
    ;;
esac

if [[ ! -f "$RPM_PATH" ]]; then
  echo "RPM package not found: $RPM_PATH" >&2
  exit 1
fi

dnf install -y coreutils file findutils grep procps-ng rpm systemd

echo "=== RPM package metadata ==="
file "$RPM_PATH"
rpm -qip "$RPM_PATH"
rpm -qp --queryformat 'Name: %{NAME}\nVersion: %{VERSION}\nRelease: %{RELEASE}\nArchitecture: %{ARCH}\n' "$RPM_PATH"
echo

ACTUAL_PACKAGE=$(rpm -qp --queryformat '%{NAME}' "$RPM_PATH")
if [[ "$ACTUAL_PACKAGE" != "$PACKAGE_NAME" ]]; then
  echo "Expected package $PACKAGE_NAME, got $ACTUAL_PACKAGE" >&2
  exit 1
fi

if [[ -n "$EXPECTED_VERSION" ]]; then
  echo "Expected release workflow version: $EXPECTED_VERSION"
fi

echo "=== RPM package files ==="
rpm -qpl "$RPM_PATH" | sort

dnf install -y "$RPM_PATH"

INSTALLED_NEVRA=$(rpm -q "$PACKAGE_NAME")
echo "Installed RPM: $INSTALLED_NEVRA"

echo "=== Installed package files ==="
rpm -ql "$PACKAGE_NAME" | sort

for command_name in "$CLI_NAME" "$DAEMON_NAME" "$MCP_NAME"; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "Expected command missing from PATH: $command_name" >&2
    exit 1
  fi
  "$command_name" --version
done

for binary_name in "$CLI_NAME" "$DAEMON_NAME" "$MCP_NAME"; do
  if ! rpm -ql "$PACKAGE_NAME" | grep -E "/${binary_name}$" >/dev/null; then
    echo "Expected packaged binary missing: $binary_name" >&2
    exit 1
  fi
done

if ! rpm -ql "$PACKAGE_NAME" | grep -E "/${APP_NAME}\.desktop$" >/dev/null; then
  echo "Expected desktop file missing for $APP_NAME" >&2
  exit 1
fi

WORKDIR=$(mktemp -d)
trap 'rm -rf "$WORKDIR"' EXIT

export HOME="$WORKDIR/home"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_DATA_HOME="$HOME/.local/share"
export XDG_CACHE_HOME="$HOME/.cache"
mkdir -p "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"

set +e
"$CLI_NAME" daemon doctor --fix --no-start --json > "$WORKDIR/doctor.json" 2> "$WORKDIR/doctor.stderr"
doctor_status=$?
set -e

echo "=== daemon doctor stdout ==="
cat "$WORKDIR/doctor.json"
echo
echo "=== daemon doctor stderr ==="
cat "$WORKDIR/doctor.stderr"
echo

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

if ! grep -Fq "ExecStart=" "$SERVICE_FILE"; then
  echo "service file has no ExecStart" >&2
  exit 1
fi

if ! grep -Fq "$XDG_DATA_HOME" "$SERVICE_FILE"; then
  echo "service file does not point at durable per-user data" >&2
  exit 1
fi

if ! grep -Fq "Environment=HOME=$HOME" "$SERVICE_FILE"; then
  echo "service file does not preserve HOME" >&2
  exit 1
fi
