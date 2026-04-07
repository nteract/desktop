# TypeScript Bindings (ts-rs)

TypeScript types in `src/bindings/` are auto-generated from Rust structs and enums. Do not edit them manually.

## Overview

The `ts-rs` crate generates TypeScript type definitions from Rust types annotated with `#[derive(TS)]`. This ensures type safety between the Rust daemon and TypeScript frontend.

## Source Files

| Rust File | Generated Types |
|-----------|-----------------|
| `crates/runtimed-client/src/settings_doc.rs` | `ThemeMode`, `PythonEnvType`, `UvDefaults`, `CondaDefaults`, `PixiDefaults`, `SyncedSettings` |
| `crates/runtimed-client/src/runtime.rs` | `Runtime` |

## How It Works

Rust types use `#[derive(TS)]` and `#[ts(export)]` attributes:

```rust
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SyncedSettings {
    pub theme: ThemeMode,
    pub default_runtime: Runtime,
    pub default_python_env: PythonEnvType,
    // ...
}
```

This generates:

```typescript
// src/bindings/ThemeMode.ts
export type ThemeMode = "system" | "light" | "dark";

// src/bindings/SyncedSettings.ts
export type SyncedSettings = {
  theme: ThemeMode;
  default_runtime: Runtime;
  default_python_env: PythonEnvType;
  // ...
};
```

## Configuration

The export directory is configured in `.cargo/config.toml`:

```toml
[env]
TS_RS_EXPORT_DIR = { value = "src/bindings", relative = true }
```

## Regenerating Bindings

Run `cargo test` to regenerate bindings:

```bash
cargo test
```

The ts-rs procedural macro exports types during test compilation. Generated files appear in `src/bindings/`.

## Adding New Bindings

1. Add the `ts-rs` dependency to your crate's `Cargo.toml`:
   ```toml
   [dependencies]
   ts-rs = { version = "12", features = ["serde-compat"] }
   ```

2. Annotate your type:
   ```rust
   use ts_rs::TS;

   #[derive(TS)]
   #[ts(export)]
   pub struct MyNewType {
       pub field: String,
   }
   ```

3. Run `cargo test` to generate the TypeScript file

4. Import from `src/bindings/index.ts`:
   ```typescript
   import type { MyNewType } from "@/bindings";
   ```

## Generated Files

```
src/bindings/
├── CondaDefaults.ts
├── PixiDefaults.ts
├── PythonEnvType.ts
├── Runtime.ts
├── SyncedSettings.ts
├── ThemeMode.ts
├── UvDefaults.ts
└── index.ts          ← Re-exports all types
```

The `index.ts` file re-exports all generated types for convenient imports.

## Usage in Frontend

```typescript
import type { ThemeMode } from "@/bindings";
import { useSyncedSettings } from "@/hooks/useSyncedSettings";

function Settings() {
  const { theme, setTheme, defaultRuntime, setDefaultRuntime } = useSyncedSettings();

  const handleThemeChange = (newTheme: ThemeMode) => {
    setTheme(newTheme);
  };
  // ...
}
```

## Key Files

| File | Role |
|------|------|
| `.cargo/config.toml` | Sets `TS_RS_EXPORT_DIR` |
| `crates/runtimed-client/src/settings_doc.rs` | Main source of settings types |
| `crates/runtimed-client/src/runtime.rs` | Runtime enum |
| `src/bindings/index.ts` | Re-exports all generated types |
| `src/hooks/useSyncedSettings.ts` | Consumes the generated types |
