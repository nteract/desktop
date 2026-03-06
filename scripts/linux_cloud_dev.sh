#!/bin/bash
set -e

# Only run in remote environments
if [ "$CLAUDE_CODE_REMOTE" != "true" ]; then
  exit 0
fi

# Tauri system deps (GTK, WebKit, etc.)
sudo apt-get update
sudo apt-get install -y \
  libgtk-3-dev \
  libwebkit2gtk-4.1-dev \
  libxdo-dev \
  libssl-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  build-essential \
  pkg-config

# Rust (pinned via rust-toolchain.toml to 1.90.0)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"

# Node 20 + pnpm 10.30.0
curl -fsSL https://fnm.vercel.app/install | bash
export PATH="$HOME/.local/share/fnm:$PATH"
eval "$(fnm env)"
fnm install 20
fnm use 20
corepack enable
corepack prepare pnpm@10.30.0 --activate

# Install deps
pnpm install
cargo fetch
exit 0
