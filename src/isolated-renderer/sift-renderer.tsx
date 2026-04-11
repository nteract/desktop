/**
 * Sift Renderer Plugin
 *
 * On-demand renderer plugin for application/vnd.apache.parquet outputs.
 * Loads parquet bytes via nteract-predicate WASM and renders with sift.
 * Loaded into the isolated iframe via the renderer plugin API.
 */

import { SiftTable } from "@nteract/sift";
import "@nteract/sift/style.css";

interface RendererProps {
  data: unknown;
  metadata?: Record<string, unknown>;
  mimeType: string;
}

function SiftRenderer({ data }: RendererProps) {
  const url = String(data);
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
