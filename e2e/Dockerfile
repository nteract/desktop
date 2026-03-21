# Dockerfile for Tauri E2E testing
# Enables running E2E tests on macOS via Docker (Linux container)
#
# Uses cargo-chef for proper Rust dependency caching — only rebuilds deps
# when Cargo.toml/Cargo.lock change, not on every source code change.
#
# Usage:
#   docker compose run --rm tauri-e2e
#   pnpm test:e2e:docker

# --- Stage 1: Install cargo-chef ---
FROM ivangabriele/tauri:debian-bookworm-20 AS chef
RUN cargo install cargo-chef --locked
WORKDIR /app

# --- Stage 2: Compute dependency recipe ---
# This layer only changes when Cargo.toml or Cargo.lock change
FROM chef AS planner
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/ crates/
RUN cargo chef prepare --recipe-path recipe.json

# --- Stage 3: Build everything ---
FROM chef AS builder

# Install test dependencies (xvfb for headless display, webkit2gtk-driver for WebDriver)
RUN apt-get update && apt-get install -y \
    xvfb \
    webkit2gtk-driver \
    && rm -rf /var/lib/apt/lists/*

# Install tauri-cli (cached unless base image changes)
RUN cargo install tauri-cli --locked

# Build Rust dependencies from recipe (cached unless Cargo.toml/Cargo.lock change)
# Override release profile for fast E2E builds (disable LTO, use more codegen units)
COPY --from=planner /app/recipe.json recipe.json
ENV CARGO_PROFILE_RELEASE_LTO=false
ENV CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
ENV CARGO_PROFILE_RELEASE_OPT_LEVEL=1
RUN cargo chef cook --release --recipe-path recipe.json

# Install JS dependencies (cached unless package files change)
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY apps/notebook/package.json apps/notebook/
COPY apps/sidecar/package.json apps/sidecar/
RUN corepack enable && pnpm install --frozen-lockfile

# Copy source code (this layer changes frequently — everything above is cached)
COPY . .

# Build frontend assets (isolated-renderer built inline via Vite plugin)
RUN pnpm --dir apps/notebook build

# Generate icons (needed for bundling)
RUN cargo xtask icons

# Build Tauri app
# --ci flag sets CI mode, --config disables beforeBuildCommand (frontend already built)
WORKDIR /app/crates/notebook
RUN cargo tauri build --ci --config '{"build":{"beforeBuildCommand":""}}'
WORKDIR /app

# Expose WebDriver port (embedded in app via tauri-plugin-webdriver, default 4445)
EXPOSE 4445

# Default command starts Xvfb and the app (which serves WebDriver on port 4445)
CMD ["bash", "-c", "Xvfb :99 -screen 0 1920x1080x24 & sleep 2 && DISPLAY=:99 ./target/release/notebook"]
