# Build & Dependency Diagram

This document shows how the crates, frontend apps, and final artifacts depend on
each other. The key insight: the Notebook app (Tauri) bundles `runtimed` and
`runt` as sidecar binaries, so those must be built **before** the Tauri bundle
step. Similarly, frontend assets must be built before their consuming Rust crates
compile.

> **Note:** `cargo xtask dev` handles the sidecar binary build automatically
> in development. For release builds the dependency chain below still applies.

## Full Build Dependency Graph

```mermaid
graph TD
    subgraph "Frontend Assets (pnpm / Vite)"
        NUI["notebook-ui<br/><i>apps/notebook/</i><br/>(includes isolated-renderer)"]
    end

    subgraph "Rust Crates (Cargo workspace)"
        ND["notebook-doc<br/><i>shared Automerge doc ops</i>"]
        RTC["runtimed-client<br/><i>shared client/runtime helpers</i>"]
        RD["runtimed (lib + bin)<br/><i>daemon</i>"]
        RC["runt (bin: runt)<br/><i>CLI</i>"]
        NB["notebook (Tauri app)<br/><i>main app</i>"]
        XT["xtask<br/><i>build orchestrator</i>"]
        RWASM["runtimed-wasm<br/><i>WASM notebook doc ops</i>"]
        RDPY["runtimed-py<br/><i>Python bindings</i>"]
    end

    subgraph "Bundled Artifacts"
        APP["Notebook .app / .dmg<br/>.AppImage / .exe"]
        PY["Python wheel<br/><i>pip install --pre runtimed</i>"]
    end

    %% Frontend → Rust compile-time dependencies
    NUI -->|"tauri beforeBuildCommand"| NB
    RWASM -->|"wasm-pack output in<br/>apps/notebook/src/wasm/"| NUI

    %% Rust crate dependencies (path deps in Cargo.toml)
    ND -->|"path dep"| RD
    ND -->|"path dep"| RWASM
    ND -->|"path dep"| RDPY
    ND -->|"path dep"| RTC
    RTC -->|"path dep"| NB
    RTC -->|"path dep"| RC
    RTC -->|"path dep"| RD
    RTC -->|"path dep"| RDPY

    %% External binary bundling (not a Cargo dep — a Tauri bundle dep)
    RD -.->|"binary copied to<br/>crates/notebook/binaries/"| APP
    RC -.->|"binary copied to<br/>crates/notebook/binaries/"| APP
    NB -->|"cargo tauri build"| APP

    %% Python package
    RDPY -->|"maturin build<br/>(bindings = pyo3)"| PY

    %% xtask orchestrates everything
    XT -.->|"orchestrates"| NUI
    XT -.->|"builds + copies<br/>runtimed & runt binaries"| APP

    classDef frontend fill:#e1f5fe,stroke:#0288d1
    classDef rust fill:#fff3e0,stroke:#ef6c00
    classDef artifact fill:#e8f5e9,stroke:#2e7d32

    class NUI frontend
    class ND,RTC,RD,RC,NB,XT,RWASM,RDPY rust
    class APP,PY artifact
```

## Build Order (step by step)

The `cargo xtask build` / `cargo xtask build-app` commands automate this, but
here is what happens under the hood:

```mermaid
graph LR
    A["1. pnpm install"] --> M["2. Build MCP widget HTML<br/>crates/runt-mcp/assets/_output.html"]
    M --> R["3. cargo build<br/>-p runtimed -p runt -p mcp-supervisor -p notebook"]
    R --> E["4. Copy sidecar binaries<br/>for Tauri bundling"]
    E --> P["5. In parallel:<br/>uv sync + maturin develop<br/>and pnpm frontend build"]
    P --> F["6. cargo tauri build<br/>or debug link step"]

    classDef step fill:#f3e5f5,stroke:#7b1fa2
    class A,M,R,E,P,F step
```

## Rust Crate Dependency Graph

Shows only the Cargo `path` dependencies between workspace members:

```mermaid
graph BT
    RD["runtimed"]
    RTC["runtimed-client"]
    RC["runt"]
    NB["notebook"]
    ND["notebook-doc"]
    NP["notebook-protocol"]
    NS["notebook-sync"]
    XT["xtask"]
    KL["kernel-launch"]
    KE["kernel-env"]
    RT["runt-trust"]
    RW["runt-workspace"]
    RDPY["runtimed-py"]
    RWASM["runtimed-wasm"]

    NB -->|"depends on"| RTC
    NB -->|"depends on"| ND
    NB -->|"depends on"| NP
    NB -->|"depends on"| NS
    NB -->|"depends on"| RT
    NB -->|"depends on"| RW
    RC -->|"depends on"| RTC
    RC -->|"depends on"| ND
    RC -->|"depends on"| RW
    RC -->|"depends on"| KE
    RD -->|"depends on"| ND
    RD -->|"depends on"| NP
    RD -->|"depends on"| RTC
    RD -->|"depends on"| KL
    RD -->|"depends on"| KE
    RD -->|"depends on"| RT
    RD -->|"depends on"| RW
    RDPY -->|"depends on"| RTC
    RDPY -->|"depends on"| ND
    RDPY -->|"depends on"| NP
    RDPY -->|"depends on"| NS
    RDPY -->|"depends on"| RW
    RDPY -->|"depends on"| KE
    RWASM -->|"depends on"| ND
    NP -->|"depends on"| ND
    NP -->|"depends on"| KE
    NS -->|"depends on"| ND
    NS -->|"depends on"| NP
    NS -->|"depends on"| RTC
    RTC -->|"depends on"| ND
    RTC -->|"depends on"| NP
    RTC -->|"depends on"| RW

    classDef standalone fill:#fff9c4,stroke:#f9a825
    classDef leaf fill:#c8e6c9,stroke:#388e3c
    classDef shared fill:#e3f2fd,stroke:#1976d2

    class KL,KE,RT,RW,RWASM standalone
    class XT standalone
    class NB,RC,RDPY leaf
    class RD,RTC,ND,NP,NS shared
```

## Key Points

| Constraint | Why |
|---|---|
| `notebook-ui` must build before Tauri bundle | `tauri.conf.json` `beforeBuildCommand` runs `pnpm --dir apps/notebook build` |
| `runtimed` + `runt` binaries must exist in `crates/notebook/binaries/` | `tauri.conf.json` lists them in `bundle.externalBin` — Tauri bundles them into the .app/.dmg/.exe |
| `isolated-renderer` built inline | The notebook-ui Vite plugin builds the isolated renderer and embeds it as a virtual module — no separate build step needed |
| `xtask` depends on `dirs`, `runt-workspace`, `serde_json` | It shells out to `cargo build`, `pnpm`, and `cargo tauri` but also reads workspace config via `runt-workspace` and resolves paths via `dirs` |
| `runtimed-wasm` must build before `notebook-ui` | wasm-pack output lands in `apps/notebook/src/wasm/runtimed-wasm/`; Vite imports it at build time. Artifacts are committed to the repo, so this step is only needed when changing `crates/runtimed-wasm/`. |
| Python wheel uses maturin | `python/runtimed/pyproject.toml` points `maturin` at `crates/runtimed-py/Cargo.toml` with `bindings = "pyo3"` |
| `notebook-doc` is shared | `crates/notebook-doc/` provides Automerge document operations used by `runtimed`, `runtimed-wasm`, and `runtimed-py` — the single source of truth for cell mutations |
