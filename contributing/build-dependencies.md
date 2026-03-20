# Build & Dependency Diagram

This document shows how the crates, frontend apps, and final artifacts depend on
each other. The key insight: the Notebook app (Tauri) bundles `runtimed` and
`runt` as sidecar binaries, so those must be built **before** the Tauri bundle
step. Similarly, frontend assets must be built before their consuming Rust crates
compile.

> **Note:** PR [#209](https://github.com/runtimed/runt/pull/209) improves the
> dev workflow so `cargo xtask dev` handles the sidecar binary build
> automatically, but for release builds the dependency chain below still applies.

## Full Build Dependency Graph

```mermaid
graph TD
    subgraph "Frontend Assets (pnpm / Vite)"
        SUI["sidecar-ui<br/><i>apps/sidecar/</i>"]
        NUI["notebook-ui<br/><i>apps/notebook/</i><br/>(includes isolated-renderer)"]
    end

    subgraph "Rust Crates (Cargo workspace)"
        TJ["tauri-jupyter<br/><i>shared Jupyter types</i>"]
        RD["runtimed (lib + bin)<br/><i>daemon</i>"]
        SC["sidecar (lib + bin)<br/><i>output viewer</i>"]
        RC["runt-cli (bin: runt)<br/><i>CLI</i>"]
        NB["notebook (Tauri app)<br/><i>main app</i>"]
        XT["xtask<br/><i>build orchestrator</i>"]
    end

    subgraph "Bundled Artifacts"
        APP["Notebook .app / .dmg<br/>.AppImage / .exe"]
        PY["Python wheel<br/><i>pip install runtimed</i>"]
    end

    %% Frontend → Rust compile-time dependencies
    SUI -->|"build.rs panics<br/>if dist/ missing"| SC
    NUI -->|"tauri beforeBuildCommand"| NB

    %% Rust crate dependencies (path deps in Cargo.toml)
    TJ -->|"path dep"| SC
    TJ -->|"path dep"| NB
    RD -->|"path dep"| NB
    RD -->|"path dep"| RC
    SC -->|"path dep"| RC

    %% External binary bundling (not a Cargo dep — a Tauri bundle dep)
    RD -.->|"binary copied to<br/>crates/notebook/binaries/"| APP
    RC -.->|"binary copied to<br/>crates/notebook/binaries/"| APP
    NB -->|"cargo tauri build"| APP

    %% Python package
    RC -->|"maturin build<br/>(bindings = bin)"| PY
    SUI -->|"embedded via<br/>rust-embed in sidecar crate"| PY

    %% xtask orchestrates everything
    XT -.->|"orchestrates"| SUI
    XT -.->|"orchestrates"| NUI
    XT -.->|"builds + copies<br/>runtimed & runt binaries"| APP

    classDef frontend fill:#e1f5fe,stroke:#0288d1
    classDef rust fill:#fff3e0,stroke:#ef6c00
    classDef artifact fill:#e8f5e9,stroke:#2e7d32

    class SUI,NUI frontend
    class TJ,RD,SC,RC,NB,XT rust
    class APP,PY artifact
```

## Build Order (step by step)

The `cargo xtask build` / `cargo xtask build-app` commands automate this, but
here is what happens under the hood:

```mermaid
graph LR
    A["1. pnpm install"] --> B["2. pnpm --dir apps/sidecar build"]
    A --> C["3. pnpm --dir apps/notebook build<br/>(includes isolated-renderer)"]
    B --> D["4. cargo build --release<br/>-p runtimed -p runt-cli"]
    C --> D
    D --> E["5. Copy binaries to<br/>crates/notebook/binaries/"]
    E --> F["6. cargo tauri build"]

    classDef step fill:#f3e5f5,stroke:#7b1fa2
    class A,B,C,D,E,F step
```

## Rust Crate Dependency Graph

Shows only the Cargo `path` dependencies between workspace members:

```mermaid
graph BT
    TJ["tauri-jupyter"]
    RD["runtimed"]
    SC["sidecar"]
    RC["runt-cli"]
    NB["notebook"]
    XT["xtask"]

    SC -->|"depends on"| TJ
    NB -->|"depends on"| TJ
    NB -->|"depends on"| RD
    RC -->|"depends on"| SC
    RC -->|"depends on"| RD

    classDef standalone fill:#fff9c4,stroke:#f9a825
    classDef leaf fill:#c8e6c9,stroke:#388e3c

    class TJ,RD standalone
    class XT standalone
    class NB,RC leaf
```

## Key Points

| Constraint | Why |
|---|---|
| `sidecar-ui` must build before `sidecar` crate | `build.rs` panics if `apps/sidecar/dist/index.html` is missing — the UI is embedded via `rust-embed` |
| `notebook-ui` must build before Tauri bundle | `tauri.conf.json` `beforeBuildCommand` runs `pnpm --dir apps/notebook build` |
| `runtimed` + `runt` binaries must exist in `crates/notebook/binaries/` | `tauri.conf.json` lists them in `bundle.externalBin` — Tauri bundles them into the .app/.dmg/.exe |
| `isolated-renderer` built inline | The notebook-ui Vite plugin builds the isolated renderer and embeds it as a virtual module — no separate build step needed |
| `xtask` has no Cargo deps | It shells out to `cargo build`, `pnpm`, and `cargo tauri` to orchestrate the full build |
| Python wheel uses maturin | `python/runtimed/pyproject.toml` points `maturin` at `crates/runt/Cargo.toml` with `bindings = "bin"` |
