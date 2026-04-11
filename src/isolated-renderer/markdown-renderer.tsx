/**
 * Markdown Renderer Plugin
 *
 * On-demand renderer plugin for text/markdown outputs. Loaded into the
 * isolated iframe via the renderer plugin API (CJS module with install()).
 *
 * This is NOT part of the core isolated renderer bundle — it's built
 * separately and injected on-demand when markdown outputs are needed.
 */

import { MarkdownOutput } from "@/components/outputs/markdown-output";
import { MathOutput } from "@/components/outputs/math-output";

interface RendererProps {
  data: unknown;
  metadata?: Record<string, unknown>;
  mimeType: string;
}

function MarkdownRenderer({ data }: RendererProps) {
  return <MarkdownOutput content={String(data)} />;
}

function LatexRenderer({ data }: RendererProps) {
  return <MathOutput content={String(data)} />;
}

export function install(ctx: {
  register: (mimeTypes: string[], component: React.ComponentType<RendererProps>) => void;
}) {
  ctx.register(["text/markdown"], MarkdownRenderer);
  ctx.register(["text/latex"], LatexRenderer);
}
