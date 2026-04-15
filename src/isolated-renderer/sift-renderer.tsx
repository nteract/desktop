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
    console.debug("[sift-renderer] setWasmUrl:", wasmUrl);
    setWasmUrl(wasmUrl);
    wasmConfigured = true;
  } catch (err) {
    console.warn("[sift-renderer] configureWasm failed, using defaults:", err);
  }
}

// --- SiftRenderer component ---

function SiftRenderer({ data, mimeType }: RendererProps) {
  const url = String(data);
  console.debug("[sift-renderer] render", { mimeType, url: url.slice(0, 120) });
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
  console.debug("[sift-renderer] plugin installed for application/vnd.apache.parquet");
  ctx.register(["application/vnd.apache.parquet"], SiftRenderer);
}
