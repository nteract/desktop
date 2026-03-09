/**
 * Renderer for application/vnd.imo.hstack+json and application/vnd.imo.vstack+json.
 *
 * Layout items are nested MIME bundles. Each item is rendered by delegating
 * to the parent OutputRenderer via the render prop.
 */

import type { ReactNode } from "react";

export interface ImoLayoutData {
  items: Array<Record<string, unknown>>;
  gap?: number;
  justify?: string;
  align?: string | null;
  wrap?: boolean;
  widths?: number[];
  heights?: number[];
}

const JUSTIFY_MAP: Record<string, string> = {
  start: "flex-start",
  center: "center",
  end: "flex-end",
  "space-between": "space-between",
  "space-around": "space-around",
};

const ALIGN_MAP: Record<string, string> = {
  start: "flex-start",
  end: "flex-end",
  center: "center",
  stretch: "stretch",
};

/**
 * Select the best MIME type from an item's bundle.
 */
const IMO_PRIORITY = [
  "application/vnd.imo.callout+json",
  "application/vnd.imo.stat+json",
  "application/vnd.imo.hstack+json",
  "application/vnd.imo.vstack+json",
  "text/markdown",
  "text/html",
  "text/plain",
];

function selectBestMime(
  bundle: Record<string, unknown>,
): { mimeType: string; data: unknown } | null {
  for (const mime of IMO_PRIORITY) {
    if (mime in bundle && bundle[mime] != null) {
      return { mimeType: mime, data: bundle[mime] };
    }
  }
  const first = Object.keys(bundle).find((k) => bundle[k] != null);
  if (first) return { mimeType: first, data: bundle[first] };
  return null;
}

export function ImoLayout({
  data,
  direction,
  renderItem,
}: {
  data: ImoLayoutData;
  direction: "row" | "column";
  renderItem: (mimeType: string, data: unknown) => ReactNode;
}) {
  const gap = data.gap ?? 0.5;
  const justify = JUSTIFY_MAP[data.justify ?? "start"] ?? "flex-start";
  const align = data.align ? (ALIGN_MAP[data.align] ?? "normal") : "normal";
  const wrap = data.wrap ? "wrap" : "nowrap";
  const sizes = direction === "row" ? data.widths : data.heights;

  return (
    <div
      style={{
        display: "flex",
        flexDirection: direction,
        justifyContent: justify,
        alignItems: align,
        flexWrap: wrap as "wrap" | "nowrap",
        gap: `${gap}rem`,
      }}
    >
      {data.items.map((item, i) => {
        const selected = selectBestMime(item);
        if (!selected) return null;

        const flexStyle = sizes?.[i] != null ? { flex: sizes[i] } : {};

        return (
          <div key={i} style={flexStyle}>
            {renderItem(selected.mimeType, selected.data)}
          </div>
        );
      })}
    </div>
  );
}
