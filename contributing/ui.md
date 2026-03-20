# UI Components (Shadcn + nteract)

This repository maintains shared UI components in `packages/ui` using the shadcn CLI and the `@nteract` registry.

## Quick Start

Run the following commands from the `packages/ui` directory:

```bash
cd packages/ui
pnpm dlx shadcn@latest registry add @nteract
pnpm dlx shadcn@latest add @nteract/all -yo
pnpm dlx shadcn@latest add @nteract/ipycanvas -yo
pnpm dlx shadcn@latest add dialog -yo
```

## Key Points

- The `components.json` file in `packages/ui` serves as the configuration source for shadcn.
- Running certain commands may generate a `deno.lock` file, though the cause remains undiagnosed.
- The `--overwrite` flag can force refresh of generated files when needed.

## Package Management Recommendation

When updating shadcn components in this project, use `pnpm` as the preferred package manager. The repository has experienced issues with `npm install` when resolving `@repo/*` workspace packages without a root JavaScript workspace configuration.
