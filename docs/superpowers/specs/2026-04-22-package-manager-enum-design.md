# Refactor: PackageManager and EnvSource enums

## Problem

Package manager identity flows as raw strings (`"uv"`, `"conda"`, `"pixi"`) through ~20 function signatures across 5 crates. Environment source labels (`"uv:inline"`, `"conda:prewarmed"`, `"pixi:toml"`) are similarly stringly-typed through auto-launch, RuntimeStateDoc, and kernel agent code.

This causes:
- Priority order mismatches between detection functions (we just fixed one today)
- Silent fallthrough to `"uv"` for unknown values (we just added `normalize_package_manager` as a band-aid)
- No exhaustive match - adding a new manager means grep, not the compiler
- 64 string comparisons against manager values in production code

## Fix

Two enums in `notebook-protocol`, introduced in two sequential PRs.

## PR 1: `PackageManager` enum

### Definition

```rust
// crates/notebook-protocol/src/connection.rs

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageManager {
    Uv,
    Conda,
    Pixi,
}
```

### Traits and methods

```rust
impl PackageManager {
    /// The canonical wire string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Uv => "uv",
            Self::Conda => "conda",
            Self::Pixi => "pixi",
        }
    }

    /// Parse with alias support. "pip" -> Uv, "mamba" -> Conda.
    pub fn parse(input: &str) -> Result<Self, String> {
        match input {
            "uv" => Ok(Self::Uv),
            "conda" => Ok(Self::Conda),
            "pixi" => Ok(Self::Pixi),
            "pip" => Ok(Self::Uv),
            "mamba" => Ok(Self::Conda),
            _ => Err(format!(
                "Unsupported package manager '{}'. Supported: uv, conda, pixi.",
                input
            )),
        }
    }
}

impl fmt::Display for PackageManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PackageManager {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}
```

### Properties

- `Copy` - three fieldless variants, no heap allocation
- `#[serde(rename_all = "lowercase")]` - serializes as `"uv"`, `"conda"`, `"pixi"` (wire-compatible)
- `parse()` absorbs `normalize_package_manager` (deleted)
- `FromStr` enables `.parse::<PackageManager>()` at all call sites
- `Display` enables `format!("{}", pm)` for logging

### What changes

| Location | Before | After |
|----------|--------|-------|
| `CreateNotebook` handshake | `package_manager: Option<String>` | `package_manager: Option<PackageManager>` |
| `connect_create` / `connect_create_relay` | `package_manager: Option<&str>` | `package_manager: Option<PackageManager>` |
| `handle_create_notebook` | `package_manager: Option<String>` | `package_manager: Option<PackageManager>` |
| `create_empty_notebook` | `package_manager: Option<&str>` | `package_manager: Option<PackageManager>` |
| `build_new_notebook_metadata` | `package_manager: Option<&str>` | `package_manager: Option<PackageManager>` |
| `detect_package_manager` (runt-mcp) | returns `String` | returns `PackageManager` |
| `detect_manager_from_metadata` | returns `Option<&'static str>` | returns `Option<PackageManager>` |
| `get_metadata_env_type` (runtimed-py) | returns `Option<String>` | returns `Option<String>` (Python boundary, converts via `.as_str()`) |
| `add_dep_for_manager` | `manager: &str` | `manager: PackageManager` |
| MCP `create_notebook` | `explicit_pkg_manager: Option<&str>` | `explicit_pkg_manager: Option<PackageManager>` |
| Python `async_client.rs` | `package_manager: Option<String>` | parses to `Option<PackageManager>` at the boundary |

### What gets deleted

- `normalize_package_manager` in `connection.rs` (absorbed by `PackageManager::parse`)
- All `match manager { "conda" => ..., "pixi" => ..., _ => ... }` patterns become `match manager { PackageManager::Conda => ..., PackageManager::Pixi => ..., PackageManager::Uv => ... }` with exhaustive checking

### Python boundary

The PyO3 boundary in `async_client.rs` accepts `Option<String>` from Python and calls `PackageManager::parse()` to convert. Returns `PyValueError` on failure. The Python-side type stays `str | None` since Python doesn't benefit from a Rust enum.

### Serde compatibility

The `CreateNotebook` handshake currently sends `"package_manager": "conda"`. With `#[serde(rename_all = "lowercase")]`, the enum serializes identically. Old clients that don't send the field get `None` via `#[serde(default)]`. Wire-compatible.

---

## PR 2: `EnvSource` enum

### Definition

```rust
// crates/notebook-protocol/src/connection.rs

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvSource {
    Prewarmed(PackageManager),
    Inline(PackageManager),
    Pyproject,              // always uv
    PixiToml,               // always pixi
    EnvYml,                 // always conda
    Pep723(PackageManager), // uv or pixi
    Deno,
}
```

### Methods

```rust
impl EnvSource {
    /// The package manager for this env source, if applicable.
    pub fn package_manager(&self) -> Option<PackageManager> {
        match self {
            Self::Prewarmed(pm) | Self::Inline(pm) | Self::Pep723(pm) => Some(*pm),
            Self::Pyproject => Some(PackageManager::Uv),
            Self::PixiToml => Some(PackageManager::Pixi),
            Self::EnvYml => Some(PackageManager::Conda),
            Self::Deno => None,
        }
    }

    /// The canonical wire string (e.g. "uv:prewarmed", "conda:inline").
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Prewarmed(PackageManager::Uv) => "uv:prewarmed",
            Self::Prewarmed(PackageManager::Conda) => "conda:prewarmed",
            Self::Prewarmed(PackageManager::Pixi) => "pixi:prewarmed",
            Self::Inline(PackageManager::Uv) => "uv:inline",
            Self::Inline(PackageManager::Conda) => "conda:inline",
            Self::Inline(PackageManager::Pixi) => "pixi:inline",
            Self::Pyproject => "uv:pyproject",
            Self::PixiToml => "pixi:toml",
            Self::EnvYml => "conda:env_yml",
            Self::Pep723(PackageManager::Uv) => "uv:pep723",
            Self::Pep723(PackageManager::Conda) => "conda:pep723",
            Self::Pep723(PackageManager::Pixi) => "pixi:pep723",
            Self::Deno => "deno",
        }
    }

    /// Parse from the wire string.
    pub fn parse(input: &str) -> Result<Self, String> {
        match input {
            "uv:prewarmed" => Ok(Self::Prewarmed(PackageManager::Uv)),
            "conda:prewarmed" => Ok(Self::Prewarmed(PackageManager::Conda)),
            "pixi:prewarmed" => Ok(Self::Prewarmed(PackageManager::Pixi)),
            "uv:inline" => Ok(Self::Inline(PackageManager::Uv)),
            "conda:inline" => Ok(Self::Inline(PackageManager::Conda)),
            "pixi:inline" => Ok(Self::Inline(PackageManager::Pixi)),
            "uv:pyproject" => Ok(Self::Pyproject),
            "pixi:toml" => Ok(Self::PixiToml),
            "conda:env_yml" => Ok(Self::EnvYml),
            "uv:pep723" => Ok(Self::Pep723(PackageManager::Uv)),
            "pixi:pep723" => Ok(Self::Pep723(PackageManager::Pixi)),
            "deno" => Ok(Self::Deno),
            _ => Err(format!("Unknown env source: '{}'", input)),
        }
    }

    /// Whether this source prepares its own env (no pool env needed).
    pub fn prepares_own_env(&self) -> bool {
        matches!(
            self,
            Self::Inline(_) | Self::Pyproject | Self::PixiToml | Self::Pep723(_)
        )
    }
}
```

### What changes

| Location | Before | After |
|----------|--------|-------|
| `check_inline_deps` | returns `Option<String>` | returns `Option<EnvSource>` |
| `auto_launch_kernel` env_source variable | `String` | `EnvSource` |
| `detect_manager_from_metadata` prewarmed construction | string match on `default_python_env` | `EnvSource::Prewarmed(pm)` |
| `prepares_own_env` check in auto-launch | 7-way string `==` comparison | `env_source.prepares_own_env()` |
| `KernelLaunched` response | `env_source: String` | `env_source: String` (wire format preserved, `.as_str()` at boundary) |
| RuntimeStateDoc `kernel.env_source` | written as string | written via `.as_str()`, read via `EnvSource::parse()` |

### Serde

`EnvSource` uses custom `Serialize`/`Deserialize` that delegate to `as_str()`/`parse()` for string-format compatibility. The RuntimeStateDoc and `KernelLaunched` response continue to carry `env_source` as a string on the wire. Internal code uses the enum.

### What gets deleted

- All `env_source.starts_with("conda:")` / `.starts_with("uv:")` prefix checks
- The 7-way `if env_source == "uv:pyproject" || env_source == "uv:inline" || ...` in auto-launch (replaced by `prepares_own_env()`)
- String construction like `format!("{}:prewarmed", ...)` in the prewarmed fallback

---

## Crates affected

| Crate | PR 1 (PackageManager) | PR 2 (EnvSource) |
|-------|----------------------|------------------|
| `notebook-protocol` | Enum definition, delete `normalize_package_manager` | Enum definition |
| `notebook-sync` | `connect_create` signatures | - |
| `runtimed` | `daemon.rs`, `load.rs`, `metadata.rs` | `metadata.rs` (auto-launch), `daemon.rs` (launch handler) |
| `runt-mcp` | `session.rs`, `deps.rs` | - |
| `runtimed-py` | `async_client.rs`, `session_core.rs`, `async_session.rs` | `session_core.rs` (env_type detection) |
| `notebook` (Tauri) | `lib.rs` (relay signature) | - |
| `runtimed-node` | `session.rs` (signature) | - |

## Testing

- Existing tests continue to pass (wire format unchanged)
- Unit tests for `PackageManager::parse` (valid, aliases, rejection)
- Unit tests for `EnvSource::parse` / `as_str` roundtrip
- Unit tests for `EnvSource::prepares_own_env`
- Exhaustive match warnings surface any missed call sites during compilation
