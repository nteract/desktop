---
paths:
  - src/components/ui/**
---

# UI Components (Shadcn + nteract)

## Quick Start

Run from the **repository root**:

```bash
pnpm dlx shadcn@latest registry add @nteract
pnpm dlx shadcn@latest add @nteract/all -yo
pnpm dlx shadcn@latest add @nteract/ipycanvas -yo
pnpm dlx shadcn@latest add dialog -yo
```

## Project Structure

```
/
├── components.json          # shadcn configuration
├── tailwind.config.js       # Tailwind config (covers src/ and apps/)
├── src/
│   ├── components/ui/       # 24 shared shadcn components
│   └── lib/utils.ts         # cn() utility
└── apps/
    └── notebook/            # Uses @/components/ui/* via path alias
```

The notebook app accesses shared components via the `@/` path alias, which resolves to `../../src/` in `apps/notebook/tsconfig.json`.

## Updating Components from Registry

Pull latest from nteract/elements:

```bash
npx shadcn@latest add @nteract/markdown-output --overwrite
npx shadcn@latest add @nteract/ansi-output --overwrite
```

The `--overwrite` flag replaces local files with upstream versions. Do not use `--overwrite` for components with intentional local customizations.

## Local Customizations vs Upstreaming

When modifying a component locally:
1. Make the change in `src/components/`
2. Document why in a code comment if it is intentional divergence
3. Consider upstreaming if the change would benefit other consumers

**Upstream changes that:** fix bugs, improve dark mode/theme support, fix CSS variable issues, add generally useful features.

**Keep local changes that:** are specific to this project, depend on local utilities not in the registry.

## CSS Variables -- Update in TWO Places

When changing CSS variables in nteract/elements, update both:
1. `app/global.css` -- for the docs site
2. `registry.json` -- for consumers installing via shadcn

If you change a CSS variable, update both files with identical values.

## Post-Install Cleanup

### Remove "use client" Directives

Registry components include `"use client"` for Next.js compatibility. These are irrelevant for the Tauri app and cause warnings. Remove after any shadcn install:

```bash
grep -rl '"use client"' src/ | xargs -I {} sed -i '' '/^"use client";$/d' {}
npx @biomejs/biome check --fix src/
```

### Silence Dynamic Import Warnings

Some widget components use dynamic imports Vite cannot analyze. Add `/* @vite-ignore */`:

```tsx
// Before (causes warning)
return import(esm);

// After
return import(/* @vite-ignore */ esm);
```

## Shared Utilities

| Utility | Location | Purpose |
|---------|----------|---------|
| `isDarkMode()` | `@/lib/dark-mode` | Theme detection |
| `ErrorBoundary` | `@/lib/error-boundary` | Fault isolation with resetKeys |
| `cn()` | `@/lib/utils` | Class name merging (clsx + tailwind-merge) |

## Troubleshooting

**Import path mismatches:** If a component imports from a non-existent path (e.g., `@/components/themes` vs `@/components/editor/themes`), check what nteract/elements expects and either create the expected path or adjust the import locally.

**CSS variables not applying:** Ensure relevant CSS files (e.g., `src/styles/ansi.css`) are imported in `src/index.css`. Check `.dark` selector matches your dark mode implementation. Verify values match between local CSS and registry.json.

**Build errors after update:** Run `tsc -b` to catch TypeScript errors. Common issues: missing imports, path mismatches, type mismatches from component prop changes.

**Package manager:** Use `pnpm` as the preferred package manager for shadcn operations.
