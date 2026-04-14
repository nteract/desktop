/**
 * Sift Renderer Plugin
 *
 * On-demand renderer plugin for application/vnd.apache.parquet outputs.
 * Loaded into the isolated iframe via the renderer plugin API.
 *
 * Data flow: kernel outputs parquet bytes → daemon stores in blob server →
 * frontend gets blob URL → iframe loads sift plugin → SiftTable fetches
 * parquet from blob URL → WASM decodes → table renders.
 */

import { setWasmUrl, SiftTable } from "@nteract/sift";
import "@nteract/sift/style.css";

// --- Types ---

interface RendererProps {
  data: unknown;
  metadata?: Record<string, unknown>;
  mimeType: string;
}

// --- WASM configuration ---

let wasmConfigured = false;

/**
 * Extract the blob server origin from a blob URL and configure WASM
 * to load from the same server's /plugins/ route.
 */
function configureWasm(blobUrl: string): void {
  if (wasmConfigured) return;
  try {
    const parsed = new URL(blobUrl);
    const wasmUrl = `${parsed.protocol}//${parsed.host}/plugins/sift_wasm.wasm`;
    setWasmUrl(wasmUrl);
    wasmConfigured = true;
  } catch {
    // Fall back to default WASM resolution
  }
}

// --- SiftRenderer component ---

function SiftRenderer({ data }: RendererProps) {
  const url = String(data);
  configureWasm(url);

  return (
    <div style={{ height: 600, width: "100%" }}>
      <SiftTable url={url} />
    </div>
  );
}

// --- Plugin install ---

export function install(ctx: {
  register: (mimeTypes: string[], component: React.ComponentType<RendererProps>) => void;
}) {
  ctx.register(["application/vnd.apache.parquet"], SiftRenderer);
}
