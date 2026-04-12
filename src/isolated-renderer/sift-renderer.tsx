/**
 * Sift Renderer Plugin
 *
 * On-demand renderer plugin for application/vnd.apache.parquet outputs.
 * Loads parquet bytes via nteract-predicate WASM and renders with sift.
 * Loaded into the isolated iframe via the renderer plugin API.
 *
 * The WASM binary is served by the daemon's blob server at
 * /plugins/nteract-predicate.wasm. The blob server port is extracted
 * from the data URL (which is a blob server URL for the parquet file).
 */

import { setWasmUrl, SiftTable } from "@nteract/sift";
import "@nteract/sift/style.css";

const themeOverrides = `
:root, :root[data-theme="dark"], :root[data-theme="light"] {
  /* Map sift core palette to notebook design tokens */
  --page: var(--background);
  --panel: var(--card);
  --ink: var(--foreground);
  --muted: var(--muted-foreground);
  --rule: var(--border);
  --accent: var(--primary);
  --row-alt: var(--secondary);
  --pin-shadow: var(--border);

  /* Badges — use semantic green/red that work on both light and dark */
  --badge-true-bg: oklch(0.65 0.15 155 / 0.15);
  --badge-true-text: oklch(0.65 0.15 155);
  --badge-false-bg: oklch(0.60 0.18 25 / 0.15);
  --badge-false-text: oklch(0.60 0.18 25);

  /* Boolean summary bar */
  --bool-true: oklch(0.55 0.15 155);
  --bool-false: oklch(0.55 0.18 25);

  /* Neutral overlays that adapt to the background */
  --bool-null-stripe-a: var(--border);
  --bool-null-stripe-b: transparent;
  --cat-bar-track: var(--secondary);
  --filter-pill-bg: var(--secondary);
  --filter-pill-border: var(--border);
  --loading-code-bg: var(--secondary);
  --skeleton-from: var(--secondary);
  --skeleton-mid: var(--muted);
  --backdrop: oklch(0 0 0 / 0.5);
  --sheet-shadow: oklch(0 0 0 / 0.2);
}
`;

interface RendererProps {
  data: unknown;
  metadata?: Record<string, unknown>;
  mimeType: string;
}

let wasmConfigured = false;

function configureWasm(blobUrl: string) {
  if (wasmConfigured) return;
  try {
    const parsed = new URL(blobUrl);
    const wasmUrl = `${parsed.protocol}//${parsed.host}/plugins/nteract-predicate.wasm`;
    setWasmUrl(wasmUrl);
    wasmConfigured = true;
  } catch {
    // Fall back to default WASM resolution
  }
}

function SiftRenderer({ data }: RendererProps) {
  const url = String(data);
  configureWasm(url);
  return (
    <div style={{ height: 600, width: "100%" }}>
      <style>{themeOverrides}</style>
      <SiftTable parquetUrl={url} />
    </div>
  );
}

export function install(ctx: {
  register: (mimeTypes: string[], component: React.ComponentType<RendererProps>) => void;
}) {
  ctx.register(["application/vnd.apache.parquet"], SiftRenderer);
}
