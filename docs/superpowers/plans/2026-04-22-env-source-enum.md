# EnvSource Enum Refactor — Implementation Plan (PR 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace raw `env_source` strings (`"uv:inline"`, `"conda:prewarmed"`, `"pixi:toml"`, etc.) with a typed `EnvSource` enum defined in `notebook-protocol`. Delete 50+ string comparisons across `launch_kernel.rs`, `metadata.rs`, `runt-mcp`, and `runtimed-py`.

**Architecture:** Two types in `notebook-protocol::connection`. `EnvSource` is the resolved value that flows through every code path after the `auto` / `deno`-override step in `launch_kernel::handle`. `LaunchSpec` is the request-time input with `auto` / `auto:uv` / `prewarmed` variants that resolve into an `EnvSource`. Like `PackageManager`, `EnvSource` uses permissive deserialization (an `Unknown(String)` variant) so wire compatibility is preserved and future variants roll in without error. The three wire-exposed fields (`LaunchKernel.env_source`, `KernelLaunched.env_source`, `RuntimeStateDoc.kernel.env_source`) stay serialized as strings — the enum's `Serialize`/`Deserialize` produce the same byte stream we produce today.

**Tech Stack:** Rust, serde, Automerge (indirect — `KernelState.env_source` is stored as a string and this plan keeps it that way). No TypeScript or WASM changes (`apps/notebook/src/types.ts` still sees strings).

**Scope note:** PR 1 (`PackageManager`) shipped in #2053. This plan only concerns the `EnvSource` / `LaunchSpec` axis. `PackageManager` is already available as an import from `notebook_protocol::connection`.

---

## Design Overview

### Two types, one file

```rust
// crates/notebook-protocol/src/connection.rs

/// A concrete, resolved environment source.
///
/// What `KernelLaunched.env_source` and `RuntimeStateDoc.kernel.env_source`
/// carry. After the daemon resolves a launch request, every downstream
/// code path receives an `EnvSource`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EnvSource {
    Prewarmed(PackageManager),  // "uv:prewarmed" / "conda:prewarmed" / "pixi:prewarmed"
    Inline(PackageManager),     // "uv:inline" / "conda:inline" / "pixi:inline"
    Pyproject,                  // "uv:pyproject" — uv-only by definition
    PixiToml,                   // "pixi:toml"   — pixi-only
    EnvYml,                     // "conda:env_yml" — conda-only
    Pep723(PackageManager),     // "uv:pep723" / "pixi:pep723" (conda unused today, but representable)
    Deno,                       // "deno"
    Unknown(String),            // unrecognized wire strings — preserves forward compat
}

/// Request-time launch specification. The caller of `LaunchKernel` sends one
/// of these; the daemon resolves it into a concrete `EnvSource`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchSpec {
    Auto,                       // "" / "auto" / "prewarmed"  — full auto-detect
    AutoScoped(PackageManager), // "auto:uv" / "auto:conda" / "auto:pixi"
    Concrete(EnvSource),        // a specific env_source value
}
```

### Why two types

The current code takes a single `String` and branches between "this is a request-time value (auto/prewarmed/auto:uv/...)" and "this is a concrete env_source to honor as-is." Splitting them into distinct enums makes the resolution step visible in the type signature: `fn resolve(spec: LaunchSpec, ...) -> EnvSource`. No code path can accidentally hand an unresolved `auto` value to the launch routing match.

### Why `Unknown(String)` on `EnvSource`

PR 1 taught us that derived `Deserialize` on a finite enum breaks wire compat. `KernelLaunched.env_source`, `RuntimeStateDoc.kernel.env_source`, and `LaunchKernelRequest.env_source` (in `runtimed-client`) are all on the wire. A daemon running an older version might emit a value a newer client hasn't seen, or vice versa. `Unknown(String)` keeps the decoding permissive and lets call sites fall back to the historical default (`Uv`-family behavior) when needed.

### Wire format: strings, serialized as before

Custom `Serialize` / `Deserialize` round-trip the wire strings (`"uv:prewarmed"`, `"conda:env_yml"`, etc.). Nothing about the byte layout changes. `Unknown(s)` serializes back to `s` verbatim.

### What does NOT change in this PR

- `KernelState.env_source: String` on the Automerge `RuntimeStateDoc` stays as a string (schema version stays at 4). Readers convert via `EnvSource::parse(&state.kernel.env_source)` at use sites.
- `LaunchKernelRequest.env_source: String` in `runtimed-client` stays — it's a serialized RPC type. Encoders/decoders convert at the boundary.
- TypeScript / frontend consumers see no change.
- `PresenceKernel.env_source` stays as a string.

---

## File Structure

| File | Role |
|------|------|
| `crates/notebook-protocol/src/connection.rs` | Define `EnvSource`, `LaunchSpec`, parse/serialize/resolve helpers, unit tests |
| `crates/runtimed/src/requests/launch_kernel.rs` | Replace 35+ string checks with enum matches. The big file. |
| `crates/runtimed/src/notebook_sync_server/metadata.rs` | Replace 40+ string checks in bootstrap / auto-launch / pool selection |
| `crates/runtimed/src/notebook_sync_server/load.rs` | `check_inline_deps` — return `Option<EnvSource>` |
| `crates/runtimed/src/project_file.rs` | `DetectedProjectFile::to_env_source` returns `EnvSource` |
| `crates/runtimed/src/runtime_agent.rs` | Internal captures — continue writing strings via `EnvSource::as_str()` |
| `crates/runtimed/src/jupyter_kernel.rs` | `.as_str()` match on launch config — replace with enum match |
| `crates/runt-mcp/src/tools/deps.rs` | `detect_package_manager` fallback reads env_source → parse to EnvSource |
| `crates/runt-mcp/src/tools/session.rs` | `runtime_info` package_manager classification via enum |
| `crates/runt-mcp/src/project_file.rs` | Dead after PR 1 — kept alive, `env_source` returns `EnvSource` |
| `crates/runtimed-py/src/session_core.rs` | `restart_env_source` helper — classify via `EnvSource::parse` |
| `crates/runtimed-client/src/protocol.rs` | Test fixture strings; no type changes needed (wire types stay `String`) |

---

## Task 1: Define `EnvSource` and `LaunchSpec` in `notebook-protocol`

**Files:**
- Modify: `crates/notebook-protocol/src/connection.rs`

**Goal:** Additive change — define both enums with full parse / serialize / resolve support and comprehensive unit tests. No call sites move yet. This task ends with the enums available for import; nothing else uses them.

- [ ] **Step 1: Write the failing tests**

Append to the bottom of the existing `#[cfg(test)] mod tests` block in `crates/notebook-protocol/src/connection.rs` (after the existing `package_manager_*` tests):

```rust
    // -----------------------------------------------------------
    // EnvSource tests
    // -----------------------------------------------------------

    #[test]
    fn env_source_as_str_round_trips_all_variants() {
        assert_eq!(EnvSource::Prewarmed(PackageManager::Uv).as_str(), "uv:prewarmed");
        assert_eq!(EnvSource::Prewarmed(PackageManager::Conda).as_str(), "conda:prewarmed");
        assert_eq!(EnvSource::Prewarmed(PackageManager::Pixi).as_str(), "pixi:prewarmed");
        assert_eq!(EnvSource::Inline(PackageManager::Uv).as_str(), "uv:inline");
        assert_eq!(EnvSource::Inline(PackageManager::Conda).as_str(), "conda:inline");
        assert_eq!(EnvSource::Inline(PackageManager::Pixi).as_str(), "pixi:inline");
        assert_eq!(EnvSource::Pyproject.as_str(), "uv:pyproject");
        assert_eq!(EnvSource::PixiToml.as_str(), "pixi:toml");
        assert_eq!(EnvSource::EnvYml.as_str(), "conda:env_yml");
        assert_eq!(EnvSource::Pep723(PackageManager::Uv).as_str(), "uv:pep723");
        assert_eq!(EnvSource::Pep723(PackageManager::Pixi).as_str(), "pixi:pep723");
        assert_eq!(EnvSource::Deno.as_str(), "deno");
    }

    #[test]
    fn env_source_parse_valid_round_trips() {
        for s in [
            "uv:prewarmed", "conda:prewarmed", "pixi:prewarmed",
            "uv:inline", "conda:inline", "pixi:inline",
            "uv:pyproject", "pixi:toml", "conda:env_yml",
            "uv:pep723", "pixi:pep723",
            "deno",
        ] {
            let parsed = EnvSource::parse(s);
            assert_eq!(parsed.as_str(), s, "round-trip failed for {s}");
            assert!(!matches!(parsed, EnvSource::Unknown(_)));
        }
    }

    #[test]
    fn env_source_parse_unknown_captures_string() {
        let pm = EnvSource::parse("weird:future-variant");
        assert_eq!(pm, EnvSource::Unknown("weird:future-variant".to_string()));
        assert_eq!(pm.as_str(), "weird:future-variant");
    }

    #[test]
    fn env_source_parse_empty_is_unknown() {
        assert_eq!(EnvSource::parse(""), EnvSource::Unknown(String::new()));
    }

    #[test]
    fn env_source_serde_is_string() {
        let src = EnvSource::Inline(PackageManager::Conda);
        let json = serde_json::to_string(&src).unwrap();
        assert_eq!(json, "\"conda:inline\"");
        let decoded: EnvSource = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, src);
    }

    #[test]
    fn env_source_serde_unknown_round_trips_verbatim() {
        let src = EnvSource::Unknown("something:new".to_string());
        let json = serde_json::to_string(&src).unwrap();
        assert_eq!(json, "\"something:new\"");
        let decoded: EnvSource = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, src);
    }

    #[test]
    fn env_source_package_manager_for_all() {
        assert_eq!(EnvSource::Prewarmed(PackageManager::Uv).package_manager(), Some(PackageManager::Uv));
        assert_eq!(EnvSource::Inline(PackageManager::Conda).package_manager(), Some(PackageManager::Conda));
        assert_eq!(EnvSource::Pyproject.package_manager(), Some(PackageManager::Uv));
        assert_eq!(EnvSource::PixiToml.package_manager(), Some(PackageManager::Pixi));
        assert_eq!(EnvSource::EnvYml.package_manager(), Some(PackageManager::Conda));
        assert_eq!(EnvSource::Pep723(PackageManager::Pixi).package_manager(), Some(PackageManager::Pixi));
        assert_eq!(EnvSource::Deno.package_manager(), None);
        assert_eq!(EnvSource::Unknown("junk".into()).package_manager(), None);
    }

    #[test]
    fn env_source_prepares_own_env() {
        // Inline / project-file / pep723 sources prepare their own env.
        assert!(EnvSource::Inline(PackageManager::Uv).prepares_own_env());
        assert!(EnvSource::Inline(PackageManager::Conda).prepares_own_env());
        assert!(EnvSource::Inline(PackageManager::Pixi).prepares_own_env());
        assert!(EnvSource::Pyproject.prepares_own_env());
        assert!(EnvSource::PixiToml.prepares_own_env());
        assert!(EnvSource::EnvYml.prepares_own_env());
        assert!(EnvSource::Pep723(PackageManager::Uv).prepares_own_env());
        assert!(EnvSource::Pep723(PackageManager::Pixi).prepares_own_env());

        // Prewarmed variants do not prepare their own env (they come from the pool).
        assert!(!EnvSource::Prewarmed(PackageManager::Uv).prepares_own_env());
        assert!(!EnvSource::Prewarmed(PackageManager::Conda).prepares_own_env());
        assert!(!EnvSource::Prewarmed(PackageManager::Pixi).prepares_own_env());

        // Deno and Unknown don't prepare envs either — they take the "no pool" path.
        assert!(!EnvSource::Deno.prepares_own_env());
        assert!(!EnvSource::Unknown("nope".into()).prepares_own_env());
    }

    // -----------------------------------------------------------
    // LaunchSpec tests
    // -----------------------------------------------------------

    #[test]
    fn launch_spec_parse_auto_variants() {
        assert_eq!(LaunchSpec::parse(""), LaunchSpec::Auto);
        assert_eq!(LaunchSpec::parse("auto"), LaunchSpec::Auto);
        assert_eq!(LaunchSpec::parse("prewarmed"), LaunchSpec::Auto);
        assert_eq!(
            LaunchSpec::parse("auto:uv"),
            LaunchSpec::AutoScoped(PackageManager::Uv)
        );
        assert_eq!(
            LaunchSpec::parse("auto:conda"),
            LaunchSpec::AutoScoped(PackageManager::Conda)
        );
        assert_eq!(
            LaunchSpec::parse("auto:pixi"),
            LaunchSpec::AutoScoped(PackageManager::Pixi)
        );
    }

    #[test]
    fn launch_spec_parse_concrete_delegates_to_env_source() {
        assert_eq!(
            LaunchSpec::parse("uv:inline"),
            LaunchSpec::Concrete(EnvSource::Inline(PackageManager::Uv))
        );
        assert_eq!(
            LaunchSpec::parse("deno"),
            LaunchSpec::Concrete(EnvSource::Deno)
        );
    }

    #[test]
    fn launch_spec_parse_future_value_is_concrete_unknown() {
        assert_eq!(
            LaunchSpec::parse("something:new"),
            LaunchSpec::Concrete(EnvSource::Unknown("something:new".to_string()))
        );
    }

    #[test]
    fn launch_spec_auto_scope_returns_manager() {
        assert_eq!(LaunchSpec::Auto.auto_scope(), None);
        assert_eq!(
            LaunchSpec::AutoScoped(PackageManager::Conda).auto_scope(),
            Some(PackageManager::Conda)
        );
        assert_eq!(
            LaunchSpec::Concrete(EnvSource::Deno).auto_scope(),
            None
        );
    }
```

- [ ] **Step 2: Run tests — should fail to compile (types not defined)**

Run: `cargo test -p notebook-protocol --lib env_source_ launch_spec_ 2>&1 | head -30`
Expected: compile errors like `cannot find type 'EnvSource' in this scope`.

- [ ] **Step 3: Add `EnvSource` definition + helpers**

In `crates/notebook-protocol/src/connection.rs`, find the end of the `PackageManager` Deserialize impl (just after the closing `}` of `impl<'de> Deserialize<'de> for PackageManager`), and insert:

```rust
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
            "pixi:pep723" => Self::Pep723(PackageManager::Pixi),
            "deno" => Self::Deno,
            other => Self::Unknown(other.to_string()),
        }
    }

    /// The package manager associated with this env source, if any.
    ///
    /// `Deno` and `Unknown(_)` return `None`.
    pub fn package_manager(&self) -> Option<PackageManager> {
        match self {
            Self::Prewarmed(pm) | Self::Inline(pm) | Self::Pep723(pm) => Some(pm.clone()),
            Self::Pyproject => Some(PackageManager::Uv),
            Self::PixiToml => Some(PackageManager::Pixi),
            Self::EnvYml => Some(PackageManager::Conda),
            Self::Deno | Self::Unknown(_) => None,
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
            Self::Inline(_)
                | Self::Pyproject
                | Self::PixiToml
                | Self::EnvYml
                | Self::Pep723(_)
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
```

- [ ] **Step 4: Run new tests — they should pass**

Run: `cargo test -p notebook-protocol --lib env_source_ launch_spec_`
Expected: 11 new tests pass (`env_source_as_str_round_trips_all_variants`, `env_source_parse_valid_round_trips`, `env_source_parse_unknown_captures_string`, `env_source_parse_empty_is_unknown`, `env_source_serde_is_string`, `env_source_serde_unknown_round_trips_verbatim`, `env_source_package_manager_for_all`, `env_source_prepares_own_env`, `launch_spec_parse_auto_variants`, `launch_spec_parse_concrete_delegates_to_env_source`, `launch_spec_parse_future_value_is_concrete_unknown`, `launch_spec_auto_scope_returns_manager`).

- [ ] **Step 5: Run the full protocol test suite — nothing regressed**

Run: `cargo test -p notebook-protocol`
Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/notebook-protocol/src/connection.rs
git commit -m "feat(notebook-protocol): add EnvSource and LaunchSpec enums"
```

---

## Task 2: Migrate project-file detection and `check_inline_deps`

**Files:**
- Modify: `crates/runtimed/src/project_file.rs` (`DetectedProjectFile::to_env_source`)
- Modify: `crates/runt-mcp/src/project_file.rs` (`DetectedProjectFile::env_source`)
- Modify: `crates/runtimed/src/notebook_sync_server/metadata.rs` (`check_inline_deps`)

**Goal:** Return `EnvSource` from the three "classifier" helpers. These are leaf producers with no wire exposure, so the change is self-contained.

- [ ] **Step 1: Update `runtimed::project_file::to_env_source`**

Open `crates/runtimed/src/project_file.rs`. Find the `to_env_source` method (around line 29). Replace:

```rust
    /// The daemon env_source for this project file.
    pub fn to_env_source(&self) -> notebook_protocol::connection::EnvSource {
        use notebook_protocol::connection::EnvSource;
        match self.kind {
            ProjectFileKind::PyprojectToml => EnvSource::Pyproject,
            ProjectFileKind::PixiToml => EnvSource::PixiToml,
            ProjectFileKind::EnvironmentYml => EnvSource::EnvYml,
        }
    }
```

(If the method currently has a different signature or a different enum shape — e.g., `EnvironmentYml` variant name may differ — adjust the match arms to match the file's existing `ProjectFileKind` variants. Use `rg "ProjectFileKind::" crates/runtimed/src/project_file.rs` to confirm.)

Update the existing tests in that file (lines 395, 412, 422 etc.) that assert on the string return value. Change:

```rust
assert_eq!(detected.to_env_source(), "pixi:toml");
```
to:
```rust
use notebook_protocol::connection::EnvSource;
assert_eq!(detected.to_env_source(), EnvSource::PixiToml);
```

Apply to all similar assertions in the file.

- [ ] **Step 2: Update `runt-mcp::project_file::env_source`**

Open `crates/runt-mcp/src/project_file.rs`. The `env_source()` method (around line 33) currently returns `&'static str`. Replace:

```rust
    /// The daemon env_source for this project file.
    pub fn env_source(&self) -> notebook_protocol::connection::EnvSource {
        use notebook_protocol::connection::EnvSource;
        match self.kind {
            ProjectFileKind::PyprojectToml => EnvSource::Pyproject,
            ProjectFileKind::PixiToml => EnvSource::PixiToml,
        }
    }
```

(Note: `runt-mcp`'s `ProjectFileKind` has only `PyprojectToml` and `PixiToml` variants — no `EnvironmentYml`. Verify with `rg "ProjectFileKind" crates/runt-mcp/src/project_file.rs`.)

- [ ] **Step 3: Update `check_inline_deps`**

Open `crates/runtimed/src/notebook_sync_server/metadata.rs`. Find `check_inline_deps` (around line 21):

```rust
pub(crate) fn check_inline_deps(
    snapshot: &NotebookMetadataSnapshot,
) -> Option<notebook_protocol::connection::EnvSource> {
    use notebook_protocol::connection::{EnvSource, PackageManager};
    if snapshot.runt.deno.is_some() {
        return Some(EnvSource::Deno);
    }

    if let Some(ref uv) = snapshot.runt.uv {
        if !uv.dependencies.is_empty() {
            return Some(EnvSource::Inline(PackageManager::Uv));
        }
    }

    if let Some(ref pixi) = snapshot.runt.pixi {
        if !pixi.dependencies.is_empty() {
            return Some(EnvSource::Inline(PackageManager::Pixi));
        }
    }

    if let Some(ref conda) = snapshot.runt.conda {
        if !conda.dependencies.is_empty() {
            return Some(EnvSource::Inline(PackageManager::Conda));
        }
    }

    None
}
```

- [ ] **Step 4: Build + test**

Run: `cargo build --workspace --exclude runtimed-py --all-targets 2>&1 | tail -30`

Expect compile errors at the callers — they expect strings but now receive `EnvSource`. Leave them failing for Task 3.

Actually, to avoid a broken workspace between tasks, pair this task with Task 3. The edits in Task 2 and Task 3 commit together.

Skip the build check here; proceed directly to Task 3.

---

## Task 3: Migrate auto-launch decision flow in `metadata.rs`

**Files:**
- Modify: `crates/runtimed/src/notebook_sync_server/metadata.rs`

**Goal:** Replace all the `env_source ==` and `.starts_with(...)` checks in the auto-launch flow. This file owns most of the complexity (40+ comparisons). Produces `EnvSource` values internally; converts to `String` via `.as_str().to_string()` at the boundaries where it writes into `room.runtime_agent_env_path`, starting-state fields, etc.

**Approach:** Inside `metadata.rs` we keep a local `EnvSource` value for the resolution logic, then serialize back to `String` when crossing into the kernel-launch request or RuntimeStateDoc. This avoids churning the in-crate struct fields (they stay `String`). Every `== "uv:inline"` becomes a `matches!(..., EnvSource::Inline(PackageManager::Uv))`, every `.starts_with("conda:")` becomes a `matches!(..., EnvSource::Prewarmed(PackageManager::Conda) | EnvSource::Inline(PackageManager::Conda) | EnvSource::EnvYml | EnvSource::Pep723(PackageManager::Conda))` (or, more cleanly, `src.package_manager() == Some(PackageManager::Conda)`).

- [ ] **Step 1: Update the auto-launch resolution (around lines 2210-2400)**

This is the body of `auto_launch_kernel`. Locate the branch that resolves `env_source` based on kernel type and seed inputs. Replace the string-returning logic with `EnvSource`:

Find the block that starts around line 2212:
```rust
    // Determine kernel type and environment
    let (kernel_type, env_source, pooled_env) = match notebook_kernel_type.as_deref() {
        Some("deno") => {
            // Notebook is a Deno notebook (per its kernelspec)
            info!("[notebook-sync] Auto-launch: Deno kernel (notebook kernelspec)");
            ("deno", "deno".to_string(), None)
        }
```

Change the tuple so `env_source` is `EnvSource`:

```rust
    use notebook_protocol::connection::{EnvSource, PackageManager};

    // Determine kernel type and environment
    let (kernel_type, env_source, pooled_env): (&str, EnvSource, Option<PooledEnv>) =
        match notebook_kernel_type.as_deref() {
            Some("deno") => {
                info!("[notebook-sync] Auto-launch: Deno kernel (notebook kernelspec)");
                ("deno", EnvSource::Deno, None)
            }
            Some("python") => {
                // Priority: project file > inline deps > prewarmed
                let env_source: EnvSource = if let Some(ref proj) = project_source {
                    info!("[notebook-sync] Auto-launch: using project file -> {}", proj.as_str());
                    proj.clone()
                } else if let Some(ref source) = inline_source {
                    if !matches!(source, EnvSource::Deno) {
                        info!(
                            "[notebook-sync] Auto-launch: found inline deps -> {}",
                            source.as_str()
                        );
                        source.clone()
                    } else {
                        // Deno inline source for a Python notebook — fall back to prewarmed.
                        EnvSource::Prewarmed(default_prewarmed_manager(default_python_env))
                    }
                } else {
                    // No deps declared — pick a prewarmed pool based on the
                    // metadata's explicit manager section (if any), else
                    // fall back to default_python_env.
                    let manager = metadata_snapshot
                        .as_ref()
                        .and_then(detect_manager_from_metadata)
                        .unwrap_or_else(|| default_prewarmed_manager(default_python_env));
                    let prewarmed = EnvSource::Prewarmed(manager);
                    info!(
                        "[notebook-sync] Auto-launch: using prewarmed ({})",
                        prewarmed.as_str()
                    );
                    prewarmed
                };

                let pooled_env = if env_source.prepares_own_env() {
                    info!(
                        "[notebook-sync] Auto-launch: {} prepares its own env, no pool env needed",
                        env_source.as_str()
                    );
                    None
                } else {
                    match acquire_prewarmed_env_with_capture(
                        env_source.as_str(),
                        &daemon,
                        room,
                        metadata_snapshot.as_ref(),
                    )
                    .await
                    {
                        Some(env) => env,
                        None => {
                            reset_starting_state(room, None).await;
                            return;
                        }
                    }
                };
                ("python", env_source, pooled_env)
            }
            None => {
                // Unknown kernel spec: default to Python or Deno based on default_runtime.
                if matches!(inline_source, Some(EnvSource::Deno))
                    || matches!(default_runtime, crate::runtime::Runtime::Deno)
                {
                    info!("[notebook-sync] Auto-launch: Deno kernel (default or runt.deno config)");
                    ("deno", EnvSource::Deno, None)
                } else {
                    // Python defaults, same resolution as above (duplicated for clarity).
                    let env_source: EnvSource = if let Some(ref source) = project_source {
                        info!("[notebook-sync] Auto-launch: using project file -> {}", source.as_str());
                        source.clone()
                    } else if let Some(ref source) = inline_source {
                        info!("[notebook-sync] Auto-launch: found inline deps -> {}", source.as_str());
                        source.clone()
                    } else {
                        let prewarmed = EnvSource::Prewarmed(default_prewarmed_manager(default_python_env));
                        info!(
                            "[notebook-sync] Auto-launch: using prewarmed ({})",
                            prewarmed.as_str()
                        );
                        prewarmed
                    };
                    let pooled_env = if env_source.prepares_own_env() {
                        info!(
                            "[notebook-sync] Auto-launch: {} prepares its own env, no pool env needed",
                            env_source.as_str()
                        );
                        None
                    } else {
                        match acquire_prewarmed_env_with_capture(
                            env_source.as_str(),
                            &daemon,
                            room,
                            metadata_snapshot.as_ref(),
                        )
                        .await
                        {
                            Some(env) => env,
                            None => {
                                reset_starting_state(room, None).await;
                                return;
                            }
                        }
                    };
                    ("python", env_source, pooled_env)
                }
            }
            Some(other) => {
                warn!(
                    "[notebook-sync] Unknown kernel type '{}', defaulting to Python",
                    other
                );
                let prewarmed = EnvSource::Prewarmed(default_prewarmed_manager(default_python_env));
                let pooled_env = match acquire_prewarmed_env_with_capture(
                    prewarmed.as_str(),
                    &daemon,
                    room,
                    metadata_snapshot.as_ref(),
                )
                .await
                {
                    Some(env) => env,
                    None => {
                        reset_starting_state(room, None).await;
                        return;
                    }
                };
                ("python", prewarmed, pooled_env)
            }
        };
```

And add this helper at the top of the file (or in a suitable `mod` location):

```rust
fn default_prewarmed_manager(
    setting: crate::settings_doc::PythonEnvType,
) -> notebook_protocol::connection::PackageManager {
    use notebook_protocol::connection::PackageManager;
    match setting {
        crate::settings_doc::PythonEnvType::Conda => PackageManager::Conda,
        crate::settings_doc::PythonEnvType::Pixi => PackageManager::Pixi,
        _ => PackageManager::Uv,
    }
}
```

`detect_manager_from_metadata` already returns `Option<PackageManager>` (PR 1). `project_source` and `inline_source` are now `Option<EnvSource>` (Task 2).

- [ ] **Step 2: Update the types of `project_source` and `inline_source` in the surrounding function**

Search upward in `auto_launch_kernel` for where these are computed. `project_source` comes from `crate::project_file::detect_project_file(...).map(|p| p.to_env_source())` — already `EnvSource` after Task 2. `inline_source` comes from `check_inline_deps(...)` — already `Option<EnvSource>` after Task 2. So types line up.

- [ ] **Step 3: Replace the 7-way string check at `env_source == ...` for prepare_own_env (lines 2270-2276, 2340-2345)**

After Step 1, those blocks should be gone, replaced by the single `if env_source.prepares_own_env()` check. Verify:

```bash
rg 'env_source == "uv:pyproject"' crates/runtimed/src/notebook_sync_server/metadata.rs
```
Expected: no matches.

- [ ] **Step 4: Replace the remaining string checks in later blocks (lines 2399-3430)**

These are the inline-env-prep and project-file-handling blocks. Each has `if env_source == "uv:pep723"`, `else if env_source == "uv:inline"`, etc. Change to enum matches using the `use notebook_protocol::connection::{EnvSource, PackageManager};` scope.

Example transformation at line 2453:
```rust
    // BEFORE:
    let (pooled_env, inline_deps) = if env_source == "uv:pep723" {

    // AFTER:
    let (pooled_env, inline_deps) = if matches!(env_source, EnvSource::Pep723(PackageManager::Uv)) {
```

Apply the same transformation to lines:
- 2496: `env_source == "uv:inline"` → `matches!(env_source, EnvSource::Inline(PackageManager::Uv))`
- 2612: `env_source == "conda:inline"` → `matches!(env_source, EnvSource::Inline(PackageManager::Conda))`
- 2694: `env_source == "pixi:inline"` → `matches!(env_source, EnvSource::Inline(PackageManager::Pixi))`
- 2707: `env_source == "pixi:pep723"` → `matches!(env_source, EnvSource::Pep723(PackageManager::Pixi))`
- 2733: `env_source.starts_with("pixi:")` → `env_source.package_manager() == Some(PackageManager::Pixi)`
- 2751-2752: `env_source == "uv:prewarmed"` / `"conda:prewarmed"` → enum equality
- 3135: `env_source == "pixi:toml"` → `matches!(env_source, EnvSource::PixiToml)`
- 3215: `env_source == "conda:env_yml"` → `matches!(env_source, EnvSource::EnvYml)`
- 3310: `env_source == "uv:pyproject"` → `matches!(env_source, EnvSource::Pyproject)`
- 3430: `env_source == "pixi:toml" || env_source == "uv:pyproject" || env_source == "conda:env_yml"` → `matches!(env_source, EnvSource::Pyproject | EnvSource::PixiToml | EnvSource::EnvYml)`

For lines like 1627 (`if env_source == "pixi:prewarmed"`) and 1643 (`if env_source.starts_with("conda:")`) in `acquire_pool_env_for_source` — this function takes `env_source: &str` today. Change its signature to `env_source: &EnvSource` and update its body:

```rust
fn acquire_pool_env_for_source(
    env_source: &notebook_protocol::connection::EnvSource,
    // ...
) -> ... {
    use notebook_protocol::connection::{EnvSource, PackageManager};
    if matches!(env_source, EnvSource::Prewarmed(PackageManager::Pixi)) {
        // ...
    }
    // ...
    if env_source.package_manager() == Some(PackageManager::Conda) {
        // ...
    }
}
```

Update callers to pass the enum instead of a `&str`. Since `auto_launch_kernel` now owns an `EnvSource` local, this is a direct `&env_source` pass.

- [ ] **Step 5: Update `captured_env_source_override` (around line 1402)**

This function currently returns `Option<String>` and produces `"uv:prewarmed"` or `"conda:prewarmed"`. Change its signature and body:

```rust
pub(crate) fn captured_env_source_override(
    captured: &CapturedEnv,
) -> Option<notebook_protocol::connection::EnvSource> {
    use notebook_protocol::connection::{EnvSource, PackageManager};
    match captured {
        CapturedEnv::Uv { .. } => Some(EnvSource::Prewarmed(PackageManager::Uv)),
        CapturedEnv::Conda { .. } => Some(EnvSource::Prewarmed(PackageManager::Conda)),
        // Add Pixi arm if CapturedEnv has one — verify with grep.
    }
}
```

Check `rg "enum CapturedEnv" crates/runtimed/src/notebook_sync_server/metadata.rs` for the full variant list.

Update the caller in `launch_kernel.rs` (see Task 4) accordingly.

- [ ] **Step 6: Update `bootstrap_kernel_state_from_snapshot` (around line 2409)**

This function has `.starts_with("pixi:")` checks and several specific-value checks. Refactor to use enum matches:

```rust
    let env_source = EnvSource::parse(&state.kernel.env_source);
    if matches!(env_source, EnvSource::PixiToml) {
        // ...
    }
```

Apply the same pattern to all sibling blocks in the function.

- [ ] **Step 7: Update captured-env validation (around line 2761)**

```rust
match (captured, requested) {
    (CapturedEnv::Uv { .. }, EnvSource::Prewarmed(PackageManager::Uv)) => true,
    (CapturedEnv::Conda { .. }, EnvSource::Prewarmed(PackageManager::Conda)) => true,
    _ => false,
}
```

- [ ] **Step 8: At the write boundary, serialize back to `String`**

Downstream writers — RuntimeStateDoc `set_kernel_info`, kernel launch request builders, broadcast writers — still take `String`/`&str`. At each boundary, call `.as_str()` or `.as_str().to_string()`. Grep for any remaining `env_source.clone()` on a `String` local that was previously a string; those call sites now need `env_source.as_str().to_string()`.

- [ ] **Step 9: Build the workspace**

Run: `cargo build --workspace --exclude runtimed-py --all-targets 2>&1 | tail -40`

Fix any straggler compile errors. Do not commit until the workspace builds green — this task commits together with Tasks 2, 4, 5 for a single atomic compile checkpoint. If that's unworkable, build-fence using `.as_str()` conversions at the boundaries with `launch_kernel.rs` so metadata.rs can compile standalone.

Do not run tests yet.

---

## Task 4: Migrate `launch_kernel::handle` (the big file)

**Files:**
- Modify: `crates/runtimed/src/requests/launch_kernel.rs`

**Goal:** Replace the request-time classification at the top (lines 163-200) with `LaunchSpec`. Replace the dozens of `==`/`.starts_with()` checks downstream with `EnvSource` matches. At the end, the function still writes a `String` into the `KernelLaunched` response, so serialize at the boundary.

- [ ] **Step 1: Parse the request-time input as `LaunchSpec` at entry**

Near the top of `handle` (after `resolved_kernel_type` is set, around line 161), insert:

```rust
use notebook_protocol::connection::{EnvSource, LaunchSpec, PackageManager};

let spec = LaunchSpec::parse(&env_source);

// Deno kernels always use EnvSource::Deno, regardless of what was requested.
// Log a warning if the caller sent a Python env_source alongside the deno kernel.
let resolved_env_source: EnvSource = if resolved_kernel_type == "deno" {
    // Any spec that isn't already Deno-compatible logs and forces Deno.
    match &spec {
        LaunchSpec::Auto | LaunchSpec::AutoScoped(_) => {}
        LaunchSpec::Concrete(EnvSource::Deno) => {}
        LaunchSpec::Concrete(other) => {
            warn!(
                "[notebook-sync] Deno kernel requested with Python env_source '{}' — \
                 ignoring and using 'deno' instead",
                other.as_str()
            );
        }
    }
    EnvSource::Deno
} else {
    match spec {
        LaunchSpec::Auto | LaunchSpec::AutoScoped(_) => {
            let auto_scope = spec.auto_scope();
            // ... existing auto-detection body, but returning EnvSource ...
            resolve_auto(auto_scope, &metadata_snapshot, notebook_path.as_deref()).await
        }
        LaunchSpec::Concrete(env_source) => env_source,
    }
};
```

Replace the existing lines 163-200 (the deno-override block + the `if env_source == "auto" || ...` block) with this single resolver.

Extract the auto-detection body (currently inline, ~line 200-340) into a helper `resolve_auto(auto_scope: Option<PackageManager>, ...) -> EnvSource`. The helper returns a concrete `EnvSource` — no more string returns.

- [ ] **Step 2: Convert the 8-way `.as_str()` match at line 475**

This is the big dispatch on `resolved_env_source.as_str()`. Replace with a match on `EnvSource` directly:

```rust
    // BEFORE:
    let launch_result = match resolved_env_source.as_str() {
        "deno" => { ... }
        "uv:prewarmed" | "conda:prewarmed" | "pixi:prewarmed" => { ... }
        "uv:inline" => { ... }
        // ... 8 more arms
    };

    // AFTER:
    let launch_result = match &resolved_env_source {
        EnvSource::Deno => { ... }
        EnvSource::Prewarmed(_) => { ... }
        EnvSource::Inline(PackageManager::Uv) => { ... }
        EnvSource::Inline(PackageManager::Conda) => { ... }
        EnvSource::Inline(PackageManager::Pixi) => { ... }
        EnvSource::Pyproject => { ... }
        EnvSource::PixiToml => { ... }
        EnvSource::EnvYml => { ... }
        EnvSource::Pep723(PackageManager::Uv) => { ... }
        EnvSource::Pep723(PackageManager::Pixi) => { ... }
        EnvSource::Inline(PackageManager::Unknown(_))
        | EnvSource::Pep723(PackageManager::Conda)
        | EnvSource::Pep723(PackageManager::Unknown(_))
        | EnvSource::Unknown(_) => {
            // Fall back to a prewarmed env — matches the historical default
            // for unrecognized env_source strings.
            warn!(
                "[notebook-sync] Unrecognized env_source '{}', falling back to uv:prewarmed",
                resolved_env_source.as_str()
            );
            // ... reuse the uv:prewarmed body ...
        }
    };
```

- [ ] **Step 3: Replace remaining scattered `.starts_with()` / `==` checks**

Apply the same transformation pattern to every remaining line. The previous exploration catalogued:
- Line 533: `resolved_env_source.starts_with("conda:")` → `resolved_env_source.package_manager() == Some(PackageManager::Conda)`
- Lines 570, 629, 726, 799, 992, 1008: specific-value equality → enum equality
- Lines 376-378 (`resolved_env_source == "pixi:toml" || ... || "conda:env_yml"`) → `matches!(resolved_env_source, EnvSource::Pyproject | EnvSource::PixiToml | EnvSource::EnvYml)`
- Line 349: `resolved_env_source == "pixi:toml"` → `matches!(..., EnvSource::PixiToml)`
- Line 402: `resolved_env_source == "pixi:toml"` → same
- Line 414: `resolved_env_source == "conda:env_yml"` → `matches!(..., EnvSource::EnvYml)`
- Line 512: `resolved_env_source == "uv:prewarmed"` → `matches!(..., EnvSource::Prewarmed(PackageManager::Uv))`

For "set env_source from captured override" at line 241:
```rust
    // BEFORE:
    if src == "uv:prewarmed" || src == "conda:prewarmed" {
        captured_src = Some(src.to_string());
    }
    // AFTER:
    if matches!(src, EnvSource::Prewarmed(_)) {
        captured_src = Some(src.clone());
    }
```
(Note: `src` here came from `captured_env_source_override` which now returns `Option<EnvSource>`.)

- [ ] **Step 4: Serialize to `String` at the response boundary**

At the site that builds `NotebookResponse::KernelLaunched`:

```rust
    Ok(NotebookResponse::KernelLaunched {
        env_source: resolved_env_source.as_str().to_string(),
        // ... other fields ...
    })
```

Also at the call to `kernel_connection::launch` or similar that takes `env_source: String`, pass `resolved_env_source.as_str().to_string()`.

- [ ] **Step 5: Build — no errors**

Run: `cargo build -p runtimed --all-targets 2>&1 | tail -30`
Expected: clean.

- [ ] **Step 6: Run the runtimed test suite**

Run: `cargo test -p runtimed --lib 2>&1 | tail -20`
Expected: all green. If `rpc_routing.rs` or `notebook_sync_server/tests.rs` test fixtures use raw strings, they should still work — the wire format is unchanged.

- [ ] **Step 7: Commit together with Tasks 2 + 3**

This single commit covers the refactor surface in `runtimed`. Tasks 2, 3, 4 together.

```bash
git add crates/runtimed/src/ crates/runt-mcp/src/project_file.rs
git commit -m "refactor(runtimed): thread EnvSource enum through auto-launch and kernel launch"
```

---

## Task 5: Migrate `jupyter_kernel.rs` and `runtime_agent.rs`

**Files:**
- Modify: `crates/runtimed/src/jupyter_kernel.rs`
- Modify: `crates/runtimed/src/runtime_agent.rs`

**Goal:** Small cleanups — convert the internal `.as_str()` match in `jupyter_kernel::launch` (line 214) to match on `EnvSource`. In `runtime_agent.rs` the three sites (569, 687, 827) just pass a captured string through; either convert to `EnvSource::parse(...).as_str().to_string()` (round-trip for future-proofing) or leave alone since they are pure pass-throughs. Prefer leaving them alone — they don't branch on the value.

- [ ] **Step 1: `jupyter_kernel.rs` line 214 — convert to enum match**

```rust
    use notebook_protocol::connection::{EnvSource, PackageManager};
    let parsed = EnvSource::parse(&config.env_source);
    match &parsed {
        EnvSource::Deno => { /* ... */ }
        EnvSource::Prewarmed(_)
        | EnvSource::Inline(_)
        | EnvSource::Pyproject
        | EnvSource::PixiToml
        | EnvSource::EnvYml
        | EnvSource::Pep723(_) => { /* python launch ... */ }
        EnvSource::Unknown(s) => {
            warn!("[jupyter-kernel] Unknown env_source '{}', treating as Python", s);
            /* python launch fallback */
        }
    }
```

- [ ] **Step 2: `runtime_agent.rs` — no changes needed**

These sites (569, 687, 827) read a string from the running kernel and forward it. No classification happens. Skip.

- [ ] **Step 3: Build + test**

Run: `cargo build -p runtimed --all-targets 2>&1 | tail -10 && cargo test -p runtimed --lib 2>&1 | tail -5`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/runtimed/src/jupyter_kernel.rs
git commit -m "refactor(runtimed): use EnvSource::parse in jupyter_kernel launch dispatch"
```

---

## Task 6: Migrate `runt-mcp` tools

**Files:**
- Modify: `crates/runt-mcp/src/tools/deps.rs`
- Modify: `crates/runt-mcp/src/tools/session.rs`
- Modify: `crates/runt-mcp/src/tools/kernel.rs` (if it classifies env_source)

**Goal:** Replace `.starts_with()` chains in `detect_package_manager` (deps.rs:70-76) and `runtime_info` (session.rs:120-125) with `EnvSource::parse(...).package_manager()`.

- [ ] **Step 1: `deps.rs` — package manager detection fallback**

Open `crates/runt-mcp/src/tools/deps.rs`. The priority-2 fallback reads `state.kernel.env_source` as a string and uses `.starts_with()`. Replace:

```rust
    // Priority 2: env_source from running kernel (fallback for notebooks
    // with no runt metadata yet)
    if let Ok(state) = handle.get_runtime_state() {
        let src = notebook_protocol::connection::EnvSource::parse(&state.kernel.env_source);
        if let Some(pm) = src.package_manager() {
            return pm;
        }
    }
```

- [ ] **Step 2: `deps.rs` — restart env_source mapping**

Find the block (around line 217) that maps `"uv:prewarmed"` → `"auto:uv"` etc. Replace:

```rust
    use notebook_protocol::connection::{EnvSource, PackageManager};
    let prev = handle
        .get_runtime_state()
        .ok()
        .map(|s| EnvSource::parse(&s.kernel.env_source));
    let restart_env_source = match prev {
        Some(EnvSource::Prewarmed(PackageManager::Uv)) => "auto:uv".to_string(),
        Some(EnvSource::Prewarmed(PackageManager::Conda)) => "auto:conda".to_string(),
        Some(EnvSource::Prewarmed(PackageManager::Pixi)) => "auto:pixi".to_string(),
        Some(src) => src.as_str().to_string(),
        None => "auto".to_string(),
    };
```

- [ ] **Step 3: `session.rs` — runtime_info classification**

Open `crates/runt-mcp/src/tools/session.rs` (lines 114-127). Replace:

```rust
    if !state.kernel.env_source.is_empty() {
        info.insert("env_source".into(), serde_json::json!(state.kernel.env_source));
        use notebook_protocol::connection::EnvSource;
        let parsed = EnvSource::parse(&state.kernel.env_source);
        if let Some(pm) = parsed.package_manager() {
            info.insert("package_manager".into(), serde_json::json!(pm.as_str()));
        } else if matches!(parsed, EnvSource::Deno) {
            info.insert("package_manager".into(), serde_json::json!("deno"));
        }
    }
```

- [ ] **Step 4: `deps.rs` — `mode` derivation in `get_dependencies`**

Around line 322:
```rust
    use notebook_protocol::connection::EnvSource;
    let mode = match env_source.as_ref().map(|s| EnvSource::parse(s)) {
        Some(EnvSource::PixiToml)
        | Some(EnvSource::Pyproject)
        | Some(EnvSource::EnvYml) => "project",
        _ => "inline",
    };
```

- [ ] **Step 5: Build + test**

Run: `cargo build -p runt-mcp --all-targets 2>&1 | tail -10 && cargo test -p runt-mcp 2>&1 | tail -10`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/runt-mcp/src/tools/deps.rs crates/runt-mcp/src/tools/session.rs
git commit -m "refactor(runt-mcp): classify env_source via EnvSource::parse"
```

---

## Task 7: Migrate `runtimed-py::session_core::restart_env_source`

**Files:**
- Modify: `crates/runtimed-py/src/session_core.rs`

**Goal:** The helper (around line 2459) maps previous env_source to a restart request. It currently pattern-matches raw strings. Use `EnvSource` + `LaunchSpec`.

- [ ] **Step 1: Rewrite the helper**

Find `restart_env_source` (around line 2459):

```rust
fn restart_env_source(prev: Option<&str>) -> String {
    use notebook_protocol::connection::{EnvSource, PackageManager};
    let Some(prev) = prev else {
        return "auto".to_string();
    };
    match EnvSource::parse(prev) {
        EnvSource::Prewarmed(PackageManager::Uv) => "auto:uv".to_string(),
        EnvSource::Prewarmed(PackageManager::Conda) => "auto:conda".to_string(),
        EnvSource::Prewarmed(PackageManager::Pixi) => "auto:pixi".to_string(),
        other => other.as_str().to_string(),
    }
}
```

Also find the non-helper site around line 632 and apply the same transformation (it's a duplicate of the test helper logic, inlined). Extract to use the helper.

- [ ] **Step 2: Update the tests (around lines 2470-2490)**

The tests continue to pass strings; no changes required as the helper takes `Option<&str>`.

- [ ] **Step 3: Build + test**

Run: `cargo check -p runtimed-py --all-targets 2>&1 | tail -5`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/runtimed-py/src/session_core.rs
git commit -m "refactor(runtimed-py): classify prev env_source via EnvSource::parse in restart helper"
```

---

## Task 8: End-to-end verification

**Goal:** Confirm wire compatibility (existing tests pass, `KernelLaunched.env_source` still decodes correctly), real MCP create_notebook works for all three managers, clippy/lint are clean.

- [ ] **Step 1: Run the complete test suite**

```bash
cargo test -p notebook-protocol 2>&1 | tail -5
cargo test -p runtimed --lib 2>&1 | tail -5
cargo test -p runt-mcp 2>&1 | tail -5
cargo xtask integration 2>&1 | tail -30
```

Expected: all green. The integration tests create notebooks with all three managers; they exercise the full auto-launch flow through the refactored resolver.

- [ ] **Step 2: Rebuild + validate via MCP**

```
up rebuild=true
```

Then via nteract-dev MCP:

```
create_notebook(runtime="python", dependencies=["numpy"], package_manager="conda", ephemeral=true)
```

Expected: `"package_manager": "conda"` in the response. Kernel auto-launches.

```
create_notebook(runtime="python", dependencies=["pandas"], package_manager="pixi", ephemeral=true)
```

Expected: `"package_manager": "pixi"`. Kernel auto-launches.

```
create_notebook(runtime="python", dependencies=["requests"], package_manager="uv", ephemeral=true)
```

Expected: `"package_manager": "uv"`. Kernel auto-launches.

For each: call `get_dependencies()` on the same notebook; confirm the deps + `env_source` field round-trip.

- [ ] **Step 3: Clippy + lint**

```bash
cargo xtask clippy 2>&1 | tail -20
cargo xtask lint --fix 2>&1 | tail -10
```
Expected: no warnings, no diffs.

- [ ] **Step 4: Confirm no lingering raw env_source strings in production code**

```bash
rg '"uv:prewarmed"|"conda:prewarmed"|"pixi:prewarmed"|"uv:inline"|"conda:inline"|"pixi:inline"|"uv:pyproject"|"pixi:toml"|"conda:env_yml"|"uv:pep723"|"pixi:pep723"' crates/runtimed/src crates/runt-mcp/src crates/runtimed-py/src --type rust
```

Expected: only matches inside `notebook-protocol/src/connection.rs` (enum definition + tests) and test fixtures. No matches in `metadata.rs`, `launch_kernel.rs`, `jupyter_kernel.rs`, `deps.rs`, `session.rs`.

Some hits in test files are OK — those test the wire format explicitly. Flag any in production code.

- [ ] **Step 5: Sanity: wire format unchanged**

Run the protocol tests that serialize `KernelLaunched` etc.:

```bash
cargo test -p notebook-protocol -- --nocapture 2>&1 | grep -E "kernel_launched|env_source" | head -10
```

Expected: the test outputs (e.g., the `"env_source":"uv:prewarmed"` literals in test fixtures) still decode and re-encode identically.

- [ ] **Step 6: Commit (if any stragglers)**

```bash
git add -u
git commit -m "chore: final fmt/clippy cleanup after EnvSource migration"
```

---

## Testing Summary

| Suite | Why |
|-------|-----|
| `cargo test -p notebook-protocol` | Enum definitions, parse/serialize round-trips (all 12+ values, Unknown capture) |
| `cargo test -p runtimed --lib` | Auto-launch resolution, metadata bootstrap, capture validation |
| `cargo test -p runt-mcp` | Dep helpers, session_info classification |
| `cargo xtask integration` | End-to-end create_notebook with inline deps across all managers |
| Live MCP `create_notebook` | Real PyO3 + daemon + kernel launch per manager family |
| `cargo xtask clippy` | Exhaustive match coverage (new enum variants force compile-time discovery of any missed sites) |

## Deliverables

- `EnvSource` and `LaunchSpec` enums in `notebook_protocol::connection`.
- 100+ string comparisons replaced with exhaustive `matches!` / `match` on the enum.
- `check_inline_deps`, `detect_project_file(...).to_env_source()`, `captured_env_source_override` return `EnvSource` / `Option<EnvSource>`.
- Wire format byte-identical (existing `rpc_routing` and protocol tests pass unchanged).
- `RuntimeStateDoc.kernel.env_source`, `KernelLaunched.env_source`, `LaunchKernelRequest.env_source` stay serialized as `String` on the wire — readers parse at use sites.
- `Unknown(String)` variant preserves forward compatibility — a daemon receiving a future env_source value keeps working instead of erroring.

## What this plan does NOT do

- Does not change the Automerge schema (`RuntimeStateDoc` still stores strings).
- Does not change the TypeScript types in `apps/notebook/src/types.ts` — frontend still sees strings.
- Does not touch `repr-llm` or any downstream visualization code.
- Does not migrate test-only string literals (in fixtures that explicitly test the wire format).
