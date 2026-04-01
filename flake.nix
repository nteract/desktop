# USAGE:
#   nix develop          # Dev shell with all build deps
#   nix build            # Build the desktop app
#   nix run              # Build and run nteract
#   nix run .#runt       # Build and run the CLI
#   nix flake check      # Run clippy + fmt checks

{
  description = "nteract desktop — a fast, modern toolkit for Jupyter notebooks";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, crane }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        rustToolchain = pkgs.rust-bin.stable."1.94.0".default.override {
          extensions = [ "rustfmt" "clippy" "rust-analyzer" "rust-src" ];
        };

        craneLib = (crane.mkLib pkgs).overrideToolchain (_: rustToolchain);

        # Shared dependency lists

        tauriLibs = with pkgs; [
          gtk3 glib gdk-pixbuf pango cairo atk harfbuzz webkitgtk_4_1
          xdotool openssl libayatana-appindicator librsvg libsoup_3
          gst_all_1.gstreamer gst_all_1.gst-plugins-base
          gst_all_1.gst-plugins-good gst_all_1.gst-plugins-bad
          zeromq
        ];

        runtimeLibs = pkgs.lib.makeLibraryPath (tauriLibs ++ (with pkgs; [
          vulkan-loader libGL mesa libx11 libxcursor libxrandr libxi
          libxscrnsaver libxcb libxcomposite libxdamage libxext libxfixes
          libxrender libxtst
        ]));

        pkgConfigPath = pkgs.lib.makeSearchPath "lib/pkgconfig" (with pkgs; [
          gtk3.dev glib.dev gdk-pixbuf.dev pango.dev cairo.dev atk.dev
          harfbuzz.dev webkitgtk_4_1.dev openssl.dev
          libayatana-appindicator.dev librsvg.dev libsoup_3.dev
        ]);

        xdgDataDirs = builtins.concatStringsSep ":" [
          # GSettings schema dirs (GLib looks in $XDG_DATA_DIRS/glib-2.0/schemas/)
          "${pkgs.gsettings-desktop-schemas}/share/gsettings-schemas/${pkgs.gsettings-desktop-schemas.name}"
          "${pkgs.gtk3}/share/gsettings-schemas/${pkgs.gtk3.name}"
          # Standard XDG share dirs
          "${pkgs.hicolor-icon-theme}/share"
          "${pkgs.shared-mime-info}/share"
          "${pkgs.gtk3}/share"
        ];

        gschemaDirs = builtins.concatStringsSep ":" [
          "${pkgs.gsettings-desktop-schemas}/share/gsettings-schemas/${pkgs.gsettings-desktop-schemas.name}/glib-2.0/schemas"
          "${pkgs.gtk3}/share/gsettings-schemas/${pkgs.gtk3.name}/glib-2.0/schemas"
        ];

        version = "2.0.0";

        # Source filtering

        filteredSrc = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = path: type:
            let
              baseName = baseNameOf path;
              relPath = pkgs.lib.removePrefix (toString ./. + "/") path;
            in
              !(pkgs.lib.hasPrefix "target/" relPath) &&
              !(pkgs.lib.hasPrefix "node_modules/" relPath) &&
              !(pkgs.lib.hasPrefix "apps/notebook/dist/" relPath) &&
              !(pkgs.lib.hasPrefix ".direnv/" relPath) &&
              !(pkgs.lib.hasPrefix ".context/" relPath) &&
              !(pkgs.lib.hasPrefix "result" baseName && (baseName == "result" || pkgs.lib.hasPrefix "result-" baseName)) &&
              baseName != ".DS_Store" &&
              baseName != "__pycache__" &&
              true;
        };

        # JavaScript / pnpm build

        pnpmDeps = pkgs.fetchPnpmDeps {
          pname = "nteract-desktop-js";
          inherit version;
          src = filteredSrc;
          fetcherVersion = 2;
          # Update with: nix build .#pnpmDeps 2>&1 | grep 'got:'
          hash = "sha256-rTIGzAqEykzSxsR/A1X5v0jFyRMOJbZDdND3PIz1rsc=";
        };

        jsBuild = pkgs.stdenv.mkDerivation {
          pname = "nteract-desktop-js";
          inherit version;
          src = filteredSrc;

          nativeBuildInputs = with pkgs; [ nodejs_20 pnpm_10 pnpmConfigHook ];

          inherit pnpmDeps;
          HOME = "$TMPDIR/build-home";

          buildPhase = ''
            runHook preBuild
            mkdir -p "$HOME"
            pnpm install --offline --frozen-lockfile
            pnpm --dir apps/notebook build
            runHook postBuild
          '';

          installPhase = ''
            runHook preInstall
            cp -r apps/notebook/dist $out
            runHook postInstall
          '';
        };

        # Rust build (sidecars + notebook app)

        rustBuild = pkgs.rustPlatform.buildRustPackage {
          pname = "nteract-desktop-rust";
          inherit version;
          src = filteredSrc;

          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = with pkgs; [ pkg-config gobject-introspection perl git ];
          buildInputs = tauriLibs ++ [ pkgs.openssl ];

          cargoBuildFlags = [ "-p" "notebook" ];
          doCheck = false;

          preBuild = ''
            export RUNT_BUILD_CHANNEL=stable
            export TAURI_CONFIG='{"build":{"devUrl":null,"beforeDevCommand":null,"beforeBuildCommand":null}}'
            unset TAURI_DEV
            export TAURI_ENV_DEBUG=false

            # Build sidecar binaries (Tauri checks for these during notebook compilation)
            cargo build --release -p runtimed -p runt-cli

            TARGET=$(rustc --print host-tuple)
            mkdir -p crates/notebook/binaries
            cp target/release/runtimed "crates/notebook/binaries/runtimed-$TARGET"
            cp target/release/runt     "crates/notebook/binaries/runt-$TARGET"

            # Copy frontend assets (Tauri's embedder doesn't follow symlinks)
            mkdir -p apps/notebook
            cp -r ${jsBuild} apps/notebook/dist
          '';

          PKG_CONFIG_PATH = pkgConfigPath;
          GIO_MODULE_DIR  = "${pkgs.glib-networking}/lib/gio/modules";
          LD_LIBRARY_PATH = runtimeLibs;
        };

        # Final wrapped package

        nteract-desktop = pkgs.stdenv.mkDerivation {
          pname = "nteract-desktop";
          inherit version;

          src = pkgs.emptyDirectory;
          nativeBuildInputs = with pkgs; [ makeWrapper ];

          installPhase = ''
            runHook preInstall

            mkdir -p $out/bin
            cp ${rustBuild}/bin/notebook $out/bin/nteract
            cp ${rustBuild}/bin/runtimed $out/bin/runtimed
            cp ${rustBuild}/bin/runt     $out/bin/runt

            # Tauri looks for sidecars at binaries/<name>-triple> next to the main executable
            mkdir -p $out/bin/binaries
            ln -s $out/bin/runtimed "$out/bin/binaries/runtimed-${pkgs.stdenv.hostPlatform.config}"
            ln -s $out/bin/runt     "$out/bin/binaries/runt-${pkgs.stdenv.hostPlatform.config}"

            wrapProgram $out/bin/nteract \
              --prefix LD_LIBRARY_PATH : "${runtimeLibs}" \
              --prefix GIO_MODULE_DIR  : "${pkgs.glib-networking}/lib/gio/modules" \
              --prefix XDG_DATA_DIRS   : "${xdgDataDirs}" \
              --set GSETTINGS_SCHEMA_DIR "${gschemaDirs}" \
              --set WEBKIT_DISABLE_COMPOSITING_MODE "1" \
              --run 'unset RUNTIMED_WORKSPACE_PATH; unset RUNTIMED_DEV; unset LD_PRELOAD'

            wrapProgram $out/bin/runtimed \
              --set LD_LIBRARY_PATH "${runtimeLibs}" \
              --prefix PATH : "${pkgs.lib.makeBinPath (with pkgs; [ gcc pkg-config python3 python3Packages.setuptools ])}" \
              --run 'unset RUNTIMED_WORKSPACE_PATH; unset RUNTIMED_DEV'

            wrapProgram $out/bin/runt \
              --prefix LD_LIBRARY_PATH : "${runtimeLibs}" \
              --run 'unset RUNTIMED_WORKSPACE_PATH; unset RUNTIMED_DEV'

            # Desktop entry
            mkdir -p $out/share/applications
            cat > $out/share/applications/nteract.desktop << 'DESKTOP'
[Desktop Entry]
Name=nteract
Comment=A fast, modern toolkit for Jupyter notebooks
Exec=nteract %f
Icon=nteract
Terminal=false
Type=Application
Categories=Development;Science;Education;
MimeType=application/x-ipynb+json;
DESKTOP

            # Icons
            for size in 32 128 256; do
              icon="${rustBuild}/share/icons/''${size}x''${size}.png"
              if [ -f "$icon" ]; then
                mkdir -p $out/share/icons/hicolor/''${size}x''${size}/apps
                cp "$icon" $out/share/icons/hicolor/''${size}x''${size}/apps/nteract.png
              fi
            done

            runHook postInstall
          '';

          meta = with pkgs.lib; {
            description = "A fast, modern toolkit for Jupyter notebooks";
            homepage    = "https://github.com/nteract/desktop";
            license     = licenses.bsd3;
            platforms   = platforms.linux;
            mainProgram = "nteract";
          };
        };

        # Crane checks (clippy + fmt)

        craneCommonArgs = {
          src = filteredSrc;
          pname   = "nteract-desktop";
          inherit version;
          strictDeps = true;
          nativeBuildInputs = with pkgs; [ pkg-config gobject-introspection perl git ];
          buildInputs = tauriLibs ++ [ pkgs.openssl ];
          PKG_CONFIG_PATH = pkgConfigPath;
          GIO_MODULE_DIR  = "${pkgs.glib-networking}/lib/gio/modules";
          LD_LIBRARY_PATH = runtimeLibs;
        };

        cargoVendorDir = craneLib.vendorCargoDeps craneCommonArgs;
        cargoArtifacts = craneLib.buildDepsOnly (craneCommonArgs // {
          inherit cargoVendorDir;
          preBuild = ''
            mkdir -p apps/notebook
            cp -r ${jsBuild} apps/notebook/dist
          '';
        });

      in
      {
        # Packages

        packages = {
          default = nteract-desktop;
          inherit nteract-desktop jsBuild rustBuild pnpmDeps;
        };

        # Apps (nix run)

        apps.default = flake-utils.lib.mkApp {
          drv  = nteract-desktop;
          name = "nteract";
        };

        apps.runt = flake-utils.lib.mkApp {
          drv  = nteract-desktop;
          name = "runt";
        };

        # Checks (nix flake check)

        checks = {
          clippy = craneLib.cargoClippy (craneCommonArgs // {
            inherit cargoArtifacts cargoVendorDir;
            preBuild = ''
              mkdir -p apps/notebook
              cp -r ${jsBuild} apps/notebook/dist
            '';
            cargoClippyExtraArgs = "--all-targets -- -D warnings";
          });

          fmt = craneLib.cargoFmt {
            src = filteredSrc;
          };
        };

        # Dev shell

        devShells.default = pkgs.mkShell {
          name = "nteract-dev";

          nativeBuildInputs = with pkgs; [
            pkg-config
            gobject-introspection
            perl
            git
            git-lfs
            curl
            cacert
          ];

          buildInputs = with pkgs; [
            rustToolchain
            cargo-watch
            cargo-expand

            nodejs_20
            pnpm_10

            python3
            python3Packages.setuptools
            python3Packages.pip
            uv

            openssl
            biome
            wayland
          ] ++ tauriLibs;

          shellHook = ''
            export RUST_SRC_PATH="${rustToolchain}/lib/rustlib/src/rust/library"
            export LD_LIBRARY_PATH="${runtimeLibs}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
            export PKG_CONFIG_PATH="${pkgConfigPath}"
            export GIO_MODULE_DIR="${pkgs.glib-networking}/lib/gio/modules"
            export XDG_DATA_DIRS="${xdgDataDirs}''${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}"
            export GSETTINGS_SCHEMA_DIR="${gschemaDirs}"
            export WEBKIT_DISABLE_COMPOSITING_MODE="1"
            export CARGO_HOME="$PWD/.cargo"
            export PATH="$CARGO_HOME/bin:$PATH"
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
            echo "  nix build               # Build the package"
            echo "  nix run                 # Build and run nteract"
            echo "  nix flake check         # Run clippy + fmt"
            echo "  cargo xtask lint        # Format and lint"
            echo ""
          '';
        };
      }
    );
}
