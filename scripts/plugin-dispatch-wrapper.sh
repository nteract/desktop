#!/usr/bin/env sh
# Unix (macOS + Linux) plugin dispatch wrapper. Shipped as bin/nteract-mcp in
# each of the distribution plugin repos.
#
# `exec`s the right per-target binary based on `uname -sm` so Claude Code
# (or any MCP client) only talks to the real MCP server — no long-lived
# wrapper parent, signals and exit codes are transparent.
#
# Edit scripts/plugin-dispatch-wrapper.sh in nteract/desktop, not the
# copy in the distribution repo — the distribution copy is overwritten
# on every release.

set -eu

os="$(uname -s)"
arch="$(uname -m)"

case "${os}-${arch}" in
  Darwin-arm64)      target="nteract-mcp-aarch64-apple-darwin" ;;
  Darwin-x86_64)     target="nteract-mcp-x86_64-apple-darwin" ;;
  Linux-x86_64)      target="nteract-mcp-x86_64-unknown-linux-gnu" ;;
  *)
    printf 'nteract-mcp: no bundled binary for %s-%s\n' "$os" "$arch" >&2
    printf 'supported: Darwin-arm64, Darwin-x86_64, Linux-x86_64 (Unix wrapper). Windows uses nteract-mcp.cmd.\n' >&2
    exit 1
    ;;
esac

dir="$(cd "$(dirname "$0")" && pwd)"
exec "${dir}/${target}" "$@"
