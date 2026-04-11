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
    <div style={{ height: "min(600px, 80vh)", width: "100%" }}>
      <SiftTable parquetUrl={url} />
    </div>
  );
}

export function install(ctx: {
  register: (mimeTypes: string[], component: React.ComponentType<RendererProps>) => void;
}) {
  ctx.register(["application/vnd.apache.parquet"], SiftRenderer);
}
