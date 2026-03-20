# Working with nteract/elements

This project uses components from the [nteract/elements](https://github.com/nteract/elements) registry via shadcn. This guide covers how to update components, make local customizations, and contribute upstream.

## Architecture

```
nteract/elements (upstream)    →    this repo (consumer)
├── registry.json                   ├── src/components/
├── registry/outputs/               │   ├── outputs/      (from registry)
├── registry/cell/                  │   ├── cell/         (from registry)
└── registry/editor/                │   └── editor/       (from registry)
```

Components are installed via `npx shadcn add @nteract/<component>`. Once installed, they become local files we can modify.

## Updating Components

Pull the latest version from the registry:

```bash
npx shadcn@latest add @nteract/markdown-output --overwrite
npx shadcn@latest add @nteract/ansi-output --overwrite
```

The `--overwrite` flag replaces local files with upstream versions.

## Local Customizations

When you need to modify a component locally:

1. **Make the change** in `src/components/`
2. **Document why** in a code comment if the change is intentional divergence
3. **Consider upstreaming** if the change would benefit other consumers

Local changes will be overwritten by `--overwrite`. To preserve customizations:
- Don't use `--overwrite` for that component, OR
- Upstream the change to nteract/elements first

## Contributing Upstream

### When to Upstream

Upstream changes that:
- Fix bugs in the registry components
- Add generally useful features
- Improve dark mode / theme support
- Fix CSS variable issues

Keep local changes that:
- Are specific to this project's needs
- Depend on local utilities not in the registry

### How to Upstream

1. **Clone nteract/elements** (if you haven't already)

2. **Make the change** in the registry source files:
   - Component code: `registry/outputs/`, `registry/cell/`, etc.
   - CSS variables: Both `app/global.css` AND `registry.json` (see below)

3. **Test locally** by running the elements docs site:
   ```bash
   pnpm install
   pnpm dev
   ```

4. **Create a PR** to nteract/elements

5. **After merge**, update this repo:
   ```bash
   npx shadcn@latest add @nteract/<component> --overwrite
   ```

### CSS Variables (Important!)

CSS variables must be defined in **two places** in nteract/elements:

1. **`app/global.css`** — For the docs site
2. **`registry.json`** — For consumers installing via shadcn

If you change a CSS variable, update both files with identical values. See [nteract/elements contributing/css-variables.md](https://github.com/nteract/elements/blob/main/contributing/css-variables.md).

## Shared Utilities

Some utilities are shared across components:

| Utility | Location | Purpose |
|---------|----------|---------|
| `isDarkMode()` | `@/lib/dark-mode` | Theme detection |
| `ErrorBoundary` | `@/lib/error-boundary` | Fault isolation with resetKeys |
| `cn()` | `@/lib/utils` | Class name merging |

## Post-Install Cleanup

After installing or updating components from the registry, run these cleanup steps:

### Remove "use client" Directives

The registry components include `"use client"` directives for Next.js/React Server Components compatibility. These are irrelevant for our Tauri app and cause noisy warnings during the isolated renderer build.

Remove them after any shadcn install:

```bash
# Remove "use client" from all src files
grep -rl '"use client"' src/ | xargs -I {} sed -i '' '/^"use client";$/d' {}

# Format to clean up empty lines
npx @biomejs/biome check --fix src/
```

### Silence Dynamic Import Warnings

Some widget components use dynamic imports that Vite can't analyze. Add `/* @vite-ignore */` comments to suppress warnings:

```tsx
// Before (causes warning)
return import(esm);

// After (warning suppressed)
return import(/* @vite-ignore */ esm);
```

## Troubleshooting

### Import path mismatches

If a component imports from a path that doesn't exist (e.g., `@/components/themes` vs `@/components/editor/themes`):

1. Check what path nteract/elements expects
2. Either create the expected path, or adjust the import locally

### CSS variables not applying

1. Ensure `src/styles/ansi.css` (or similar) is imported in `src/index.css`
2. Check that the `.dark` selector matches your dark mode implementation
3. Verify values match between local CSS and registry.json

### Build errors after update

Run `pnpm run types:check` to catch TypeScript errors. Common issues:
- Missing imports (add the dependency)
- Path mismatches (adjust imports)
- Type mismatches (check component prop changes)
