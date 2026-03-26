# ============================================
# nteract desktop - Nix Development Environment
# ============================================
#
# QUICK START:
#   1. nix develop          # Enter the dev shell
#   2. cargo xtask dev      # Or run commands below individually
#
# DEVELOPMENT COMMANDS:
#   cargo xtask dev-daemon  # Start the dev daemon in one terminal
#   cargo xtask notebook    # Start the notebook app in another terminal
#   cargo xtask vite        # Start Vite dev server for hot-reload
#   cargo xtask run         # Run the built binary
#
# TESTING:
#   cargo test             # Run Rust tests
#   pnpm test              # Run JS tests
#   cargo xtask lint       # Format and lint everything
#
# To run everything at once (in separate terminals):
#   cargo xtask dev

{
  description = "nteract desktop - development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
          config = {
            # Permit unfree packages if needed (e.g., for some codecs)
            allowUnfree = false;
          };
        };

        # Rust toolchain - matches the project requirements
        rustToolchain = pkgs.rust-bin.stable."1.94.0".default.override {
          extensions = [ "rustfmt" "clippy" "rust-analyzer" "rust-src" ];
        };

        # Tauri dependencies for Linux
        tauriLibs = with pkgs; [
          gtk3
          glib
          gdk-pixbuf
          pango
          cairo
          atk
          harfbuzz
          webkitgtk_4_1
          xdotool
          openssl
          libayatana-appindicator
          librsvg
          libsoup_3
          # GStreamer for media playback
          gst_all_1.gstreamer
          gst_all_1.gst-plugins-base
          gst_all_1.gst-plugins-good
          gst_all_1.gst-plugins-bad
          # ZeroMQ for Jupyter kernel communication
          zeromq
        ];

        # Runtime libraries - linked into binaries and needed at runtime
        runtimeLibs = pkgs.lib.makeLibraryPath (tauriLibs ++ (with pkgs; [
          vulkan-loader
          libGL
          mesa
          libx11
          libxcursor
          libxrandr
          libxi
          libxscrnsaver
          libxcb
          libxcomposite
          libxdamage
          libxext
          libxfixes
          libxrender
          libxtst
        ]));

        # pkg-config path for build scripts
        pkgConfigPath = pkgs.lib.makeSearchPath "lib/pkgconfig" (with pkgs; [
          gtk3.dev
          glib.dev
          gdk-pixbuf.dev
          pango.dev
          cairo.dev
          atk.dev
          harfbuzz.dev
          webkitgtk_4_1.dev
          openssl.dev
          libayatana-appindicator.dev
          librsvg.dev
          libsoup_3.dev
        ]);

        # XDG data directories for schemas and icons
        xdgDataDirs = pkgs.lib.makeSearchPath "share" (with pkgs; [
          gtk3
          gsettings-desktop-schemas
          hicolor-icon-theme
          shared-mime-info
        ]);

      in
      {
        # Only provide a devShell - no packages, no apps
        devShells.default = pkgs.mkShell {
          name = "nteract-dev";

          # Build tools
          nativeBuildInputs = with pkgs; [
            pkg-config
            gobject-introspection
            perl  # Needed for some build scripts
            git
            git-lfs
            curl
            cacert
          ];

          # The actual development dependencies
          buildInputs = with pkgs; [
            # Rust
            rustToolchain
            cargo-watch
            cargo-expand

            # Node.js / pnpm
            nodejs_20
            pnpm_10

            # Python (for kernels and maturin)
            python3
            python3Packages.setuptools
            python3Packages.pip
            uv  # Python package manager

            # OpenSSL
            openssl

            # Development tools
            biome  # JS/TS linter and formatter

            # Wayland-client
            wayland
            pkg-config

            # Tauri dependencies
          ] ++ tauriLibs;

          # Environment variables
          shellHook = ''
            # Rust
            export RUST_SRC_PATH="${rustToolchain}/lib/rustlib/src/rust/library"

            # Library paths for runtime
            export LD_LIBRARY_PATH="${runtimeLibs}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
            export PKG_CONFIG_PATH="${pkgConfigPath}"
            export GIO_MODULE_DIR="${pkgs.glib-networking}/lib/gio/modules"
            export XDG_DATA_DIRS="${xdgDataDirs}''${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}"

            # GSettings schemas - note the glib-2.0/schemas subdirectory
            export GSETTINGS_SCHEMA_DIR="${pkgs.gsettings-desktop-schemas}/share/gsettings-schemas/${pkgs.gsettings-desktop-schemas.name}/glib-2.0/schemas:${pkgs.gtk3}/share/gsettings-schemas/${pkgs.gtk3.name}/glib-2.0/schemas"

            # WebKit workaround
            export WEBKIT_DISABLE_COMPOSITING_MODE="1"

            # Cargo paths
            export CARGO_HOME="$PWD/.cargo"
            export PATH="$CARGO_HOME/bin:$PATH"

            # Python
            export UV_LINK_MODE=copy

            echo ""
            echo "╔══════════════════════════════════════════════════════════╗"
            echo "║  nteract desktop - Development Environment              ║"
            echo "╚══════════════════════════════════════════════════════════╝"
            echo ""
            echo "Quick start:"
            echo "  cargo xtask dev-daemon  # Terminal 1: Start dev daemon"
            echo "  cargo xtask notebook    # Terminal 2: Start notebook app"
            echo ""
            echo "Other commands:"
            echo "  cargo xtask dev         # Run dev daemon + notebook"
            echo "  cargo xtask vite        # Start Vite dev server only"
            echo "  cargo test              # Run tests"
            echo "  cargo xtask lint        # Format and lint"
            echo ""
            echo "For more commands: cargo xtask help"
            echo ""
          '';
        };
      }
    );
}
