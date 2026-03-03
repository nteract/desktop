# UI Components (Shadcn + nteract)

This repository maintains shared UI components at the repo root using the shadcn CLI and the `@nteract` registry.

## Quick Start

Run the following commands from the **repository root**:

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
│   ├── components/ui/       # 23 shared shadcn components
│   └── lib/utils.ts         # cn() utility
└── apps/
    ├── notebook/            # Uses @/components/ui/* via path alias
    └── sidecar/             # Uses @/components/ui/* via path alias
```

Both apps access shared components via the `@/` path alias, which resolves to `../../src/` in their tsconfig.json files.

## Key Points

- The `components.json` file at the repo root configures shadcn.
- Running certain commands may generate a `deno.lock` file, though the cause remains undiagnosed.
- The `--overwrite` flag can force refresh of generated files when needed.

## Package Management Recommendation

When updating shadcn components in this project, use `pnpm` as the preferred package manager.
