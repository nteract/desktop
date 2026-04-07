---
name: typescript-bindings
description: Generate TypeScript bindings from Rust types using ts-rs. Use when adding new Rust types that need TypeScript equivalents.
---

# TypeScript Bindings (ts-rs)

## Overview

TypeScript types in `src/bindings/` are auto-generated from Rust structs and enums via the `ts-rs` crate. Do not edit them manually.

## Source Files

| Rust File | Generated Types |
|-----------|-----------------|
| `crates/runtimed-client/src/settings_doc.rs` | `ThemeMode`, `PythonEnvType`, `UvDefaults`, `CondaDefaults`, `PixiDefaults`, `SyncedSettings` |
| `crates/runtimed-client/src/runtime.rs` | `Runtime` |

## How It Works

Annotate Rust types with `#[derive(TS)]` and `#[ts(export)]`:

```rust
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum ThemeMode {
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

Generates:

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

Export directory set in `.cargo/config.toml`:

```toml
[env]
TS_RS_EXPORT_DIR = { value = "src/bindings", relative = true }
```

## Regenerating Bindings

```bash
cargo test
```

The ts-rs procedural macro exports types during test compilation. Generated files appear in `src/bindings/`.

## Adding New Bindings

1. Add `ts-rs` dependency to your crate's `Cargo.toml`:
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
  CondaDefaults.ts
  PixiDefaults.ts
  PythonEnvType.ts
  Runtime.ts
  SyncedSettings.ts
  ThemeMode.ts
  UvDefaults.ts
  index.ts          -- Re-exports all types
```

## Usage in Frontend

```typescript
import type { ThemeMode } from "@/bindings";
import { useSyncedSettings } from "@/hooks/useSyncedSettings";

function Settings() {
  const { theme, setTheme } = useSyncedSettings();
  const handleThemeChange = (newTheme: ThemeMode) => {
    setTheme(newTheme);
  };
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
