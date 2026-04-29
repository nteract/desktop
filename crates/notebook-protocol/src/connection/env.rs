//! Environment and launch-source protocol helpers.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

/// Supported package managers for Python notebooks.
///
/// The canonical wire format is the lowercase variant name (`"uv"`, `"conda"`,
/// `"pixi"`). `parse()` additionally accepts `"pip"` (→ Uv) and `"mamba"` (→
/// Conda) as aliases at user-input boundaries.
///
/// Deserialization is permissive: unrecognized wire strings land in
/// `Unknown(s)` rather than failing, so the `CreateNotebook` handshake stays
/// forward-compatible and legacy aliases still decode. Resolve `Unknown`
/// values to a canonical variant at use-site via `resolve()`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PackageManager {
    Uv,
    Conda,
    Pixi,
    /// An unrecognized package manager string, carried verbatim from the wire.
    ///
    /// Produced only by `Deserialize` for non-canonical values; internal code
    /// should call `resolve()` before matching. User-input boundaries (Python
    /// API, MCP `create_notebook`, CLI) should call `parse()` instead, which
    /// rejects unknowns up front.
    Unknown(String),
}

impl PackageManager {
    /// The wire string form.
    ///
    /// Canonical variants return their literal name; `Unknown(s)` returns the
    /// raw string that was deserialized.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Uv => "uv",
            Self::Conda => "conda",
            Self::Pixi => "pixi",
            Self::Unknown(s) => s.as_str(),
        }
    }

    /// Parse a package manager name with alias support.
    ///
    /// Accepts `"uv"`, `"conda"`, `"pixi"` (canonical), plus `"pip"` (→ Uv)
    /// and `"mamba"` (→ Conda). Returns `Err` for anything else.
    ///
    /// Use this at user-input boundaries where immediate validation is
    /// desired. Wire deserialization is permissive and never errors — see
    /// `Unknown`.
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

    /// Fold to a canonical variant, resolving known aliases.
    ///
    /// `Uv`/`Conda`/`Pixi` pass through. `Unknown("pip")` → `Uv`,
    /// `Unknown("mamba")` → `Conda`. Any other `Unknown(s)` returns `Err`.
    ///
    /// Call this at internal evaluation sites where the code needs one of the
    /// three canonical variants. Error handling is up to the caller — the
    /// daemon handshake path falls back to `default_python_env`, for example.
    pub fn resolve(&self) -> Result<Self, String> {
        match self {
            Self::Uv => Ok(Self::Uv),
            Self::Conda => Ok(Self::Conda),
            Self::Pixi => Ok(Self::Pixi),
            Self::Unknown(s) => Self::parse(s),
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

impl Serialize for PackageManager {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PackageManager {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        // Permissive: unknown strings are captured rather than rejected, so
        // the handshake stays forward-compatible and legacy aliases
        // (`"pip"`, `"mamba"`) still decode. Internal callers fold aliases
        // and reject genuine unknowns via `resolve()`.
        if raw == "uv" {
            Ok(Self::Uv)
        } else if raw == "conda" {
            Ok(Self::Conda)
        } else if raw == "pixi" {
            Ok(Self::Pixi)
        } else {
            Ok(Self::Unknown(raw))
        }
    }
}

/// A concrete, resolved environment source.
///
/// Carried on `KernelLaunched.env_source` and
/// `RuntimeStateDoc.kernel.env_source`. The daemon resolves the request-time
/// `LaunchSpec` into an `EnvSource` before routing the launch; every
/// downstream code path works against this type.
///
/// Deserialization is permissive: unrecognized wire strings land in
/// `Unknown(s)` rather than failing, so the daemon and clients stay
/// forward-compatible across versions.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EnvSource {
    /// Prewarmed pool env (e.g. `"uv:prewarmed"`). The daemon acquires these
    /// from its warming pool — they do not prepare their own env.
    Prewarmed(PackageManager),
    /// Dependencies declared in notebook metadata (e.g. `"uv:inline"`). The
    /// daemon builds the env from the metadata before kernel launch.
    Inline(PackageManager),
    /// `pyproject.toml` on disk (`"uv:pyproject"`). UV-only by definition.
    Pyproject,
    /// `pixi.toml` on disk (`"pixi:toml"`). Pixi-only.
    PixiToml,
    /// `environment.yml` on disk (`"conda:env_yml"`). Conda-only.
    EnvYml,
    /// PEP 723 script deps extracted from cell source (e.g. `"uv:pep723"`).
    Pep723(PackageManager),
    /// Deno TypeScript kernel — no Python env.
    Deno,
    /// Unrecognized wire string, preserved verbatim. Produced only by
    /// `Deserialize` for values we haven't taught the enum about. Handle this
    /// at match sites by falling back to the historical default (usually
    /// Uv-family behavior) — never panic.
    Unknown(String),
}

impl EnvSource {
    /// The wire string form.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Prewarmed(PackageManager::Uv) => "uv:prewarmed",
            Self::Prewarmed(PackageManager::Conda) => "conda:prewarmed",
            Self::Prewarmed(PackageManager::Pixi) => "pixi:prewarmed",
            Self::Prewarmed(PackageManager::Unknown(s)) => s.as_str(),
            Self::Inline(PackageManager::Uv) => "uv:inline",
            Self::Inline(PackageManager::Conda) => "conda:inline",
            Self::Inline(PackageManager::Pixi) => "pixi:inline",
            Self::Inline(PackageManager::Unknown(s)) => s.as_str(),
            Self::Pyproject => "uv:pyproject",
            Self::PixiToml => "pixi:toml",
            Self::EnvYml => "conda:env_yml",
            Self::Pep723(PackageManager::Uv) => "uv:pep723",
            Self::Pep723(PackageManager::Conda) => "conda:pep723",
            Self::Pep723(PackageManager::Pixi) => "pixi:pep723",
            Self::Pep723(PackageManager::Unknown(s)) => s.as_str(),
            Self::Deno => "deno",
            Self::Unknown(s) => s.as_str(),
        }
    }

    /// Parse a wire string. Never fails — unrecognized values land in
    /// `Unknown(s)`.
    pub fn parse(input: &str) -> Self {
        match input {
            "uv:prewarmed" => Self::Prewarmed(PackageManager::Uv),
            "conda:prewarmed" => Self::Prewarmed(PackageManager::Conda),
            "pixi:prewarmed" => Self::Prewarmed(PackageManager::Pixi),
            "uv:inline" => Self::Inline(PackageManager::Uv),
            "conda:inline" => Self::Inline(PackageManager::Conda),
            "pixi:inline" => Self::Inline(PackageManager::Pixi),
            "uv:pyproject" => Self::Pyproject,
            "pixi:toml" => Self::PixiToml,
            "conda:env_yml" => Self::EnvYml,
            "uv:pep723" => Self::Pep723(PackageManager::Uv),
            "conda:pep723" => Self::Pep723(PackageManager::Conda),
            "pixi:pep723" => Self::Pep723(PackageManager::Pixi),
            "deno" => Self::Deno,
            other => Self::Unknown(other.to_string()),
        }
    }

    /// The package manager associated with this env source, if any.
    ///
    /// For canonical variants this returns the associated manager directly.
    /// For `Unknown(s)`, the wire string's prefix is inspected so that a
    /// forward-compatible value like `"conda:foo"` or `"pixi:bar"` still
    /// routes to the correct package manager family. `Deno` and `Unknown`
    /// strings without a recognized prefix return `None`.
    pub fn package_manager(&self) -> Option<PackageManager> {
        match self {
            Self::Prewarmed(pm) | Self::Inline(pm) | Self::Pep723(pm) => Some(pm.clone()),
            Self::Pyproject => Some(PackageManager::Uv),
            Self::PixiToml => Some(PackageManager::Pixi),
            Self::EnvYml => Some(PackageManager::Conda),
            Self::Deno => None,
            Self::Unknown(s) => {
                if s.starts_with("uv:") {
                    Some(PackageManager::Uv)
                } else if s.starts_with("conda:") {
                    Some(PackageManager::Conda)
                } else if s.starts_with("pixi:") {
                    Some(PackageManager::Pixi)
                } else {
                    None
                }
            }
        }
    }

    /// True if this source prepares its own environment (no pool env needed).
    ///
    /// Used at auto-launch time to decide whether to acquire a prewarmed env
    /// from the pool. `Inline`, project-file, and `Pep723` sources build
    /// their env themselves; `Prewarmed` pulls from the pool; `Deno` and
    /// `Unknown` take the no-pool path.
    pub fn prepares_own_env(&self) -> bool {
        matches!(
            self,
            Self::Inline(_) | Self::Pyproject | Self::PixiToml | Self::EnvYml | Self::Pep723(_)
        )
    }
}

impl fmt::Display for EnvSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for EnvSource {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EnvSource {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Ok(Self::parse(&raw))
    }
}

/// Request-time launch specification.
///
/// The caller of `LaunchKernel` sends a `LaunchSpec`. The daemon resolves
/// it into a concrete `EnvSource` before routing the launch. This type
/// keeps the auto-detection inputs (`""`, `"auto"`, `"auto:uv"`,
/// `"prewarmed"`) visibly distinct from a concrete env_source in the type
/// system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchSpec {
    /// Auto-detect everything — derived from notebook metadata, project
    /// files, or PEP 723 script blocks. Wire strings: `""`, `"auto"`,
    /// `"prewarmed"` (legacy alias).
    Auto,
    /// Auto-detect within a specific package manager family. Wire strings:
    /// `"auto:uv"`, `"auto:conda"`, `"auto:pixi"`.
    AutoScoped(PackageManager),
    /// A concrete env_source to honor as-is.
    Concrete(EnvSource),
}

impl LaunchSpec {
    /// Parse a launch spec from the wire string.
    pub fn parse(input: &str) -> Self {
        match input {
            "" | "auto" | "prewarmed" => Self::Auto,
            "auto:uv" => Self::AutoScoped(PackageManager::Uv),
            "auto:conda" => Self::AutoScoped(PackageManager::Conda),
            "auto:pixi" => Self::AutoScoped(PackageManager::Pixi),
            other => Self::Concrete(EnvSource::parse(other)),
        }
    }

    /// If this spec is `AutoScoped(pm)`, returns `Some(pm)`; otherwise None.
    pub fn auto_scope(&self) -> Option<PackageManager> {
        match self {
            Self::AutoScoped(pm) => Some(pm.clone()),
            _ => None,
        }
    }
}
