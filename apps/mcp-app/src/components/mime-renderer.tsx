import { useEffect, useState } from "react";
import { fetchBlobText, isBlobUrl } from "../lib/blob-fetch";
import { selectMimeType } from "../lib/mime-priority";
import { loadPluginForMime, needsDaemonPlugin } from "../lib/plugin-loader";
import { getPluginRenderer } from "../lib/plugin-executor";
import { AnsiText } from "./ansi-text";
import { HtmlOutput } from "./html-output";
import { ImageOutput } from "./image-output";
import { JsonOutput } from "./json-output";
import type { CellOutput } from "../types";

interface MimeRendererProps {
  data: Record<string, string>;
  /** Base URL for daemon HTTP server (for fetching blob data and plugins) */
  blobBaseUrl?: string;
}

export function MimeRenderer({ data, blobBaseUrl }: MimeRendererProps) {
  const mime = selectMimeType(data);
  if (!mime) return null;
  const raw = data[mime];
  if (raw == null) return null;

  // Images: use blob URL directly as <img src>, no fetch needed
  if (mime.startsWith("image/") && mime !== "image/svg+xml") {
    return <ImageOutput data={String(raw)} mediaType={mime} alt={data["text/plain"] || undefined} />;
  }

  // text/plain fallback for when blob fetch or plugin load fails
  const plainFallback = data["text/plain"] ? String(data["text/plain"]) : undefined;

  // Plugin-rendered MIME types (markdown, latex, plotly, vega, leaflet)
  if (needsDaemonPlugin(mime)) {
    return (
      <PluginRenderer
        mime={mime}
        raw={String(raw)}
        blobBaseUrl={blobBaseUrl}
        plainFallback={plainFallback}
      />
    );
  }

  return <FetchAndRender mime={mime} raw={String(raw)} plainFallback={plainFallback} />;
}

/**
 * Load a daemon-served plugin via <script> tag and render data with it.
 * Loads plugin and fetches blob data in parallel.
 */
function PluginRenderer({
  mime,
  raw,
  blobBaseUrl,
  plainFallback,
}: {
  mime: string;
  raw: string;
  blobBaseUrl?: string;
  plainFallback?: string;
}) {
  const [data, setData] = useState<unknown>(null);
  const [pluginReady, setPluginReady] = useState(false);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;

    // Load plugin via <script> tag and fetch/parse data in parallel
    const pluginPromise = loadPluginForMime(mime, blobBaseUrl) ?? Promise.resolve();
    // Parse data: try JSON (for plotly/vega specs), fall back to raw string (for markdown/latex)
    const parseData = (text: string) => {
      try { return JSON.parse(text); } catch { return text; }
    };
    const dataPromise = isBlobUrl(raw)
      ? fetchBlobText(raw).then(parseData)
      : Promise.resolve(parseData(raw));

    Promise.all([pluginPromise, dataPromise])
      .then(([, parsedData]) => {
        if (cancelled) return;
        setPluginReady(true);
        setData(parsedData);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });

    return () => { cancelled = true; };
  }, [mime, raw, blobBaseUrl]);

  if (failed) {
    if (plainFallback) return <AnsiText text={plainFallback} />;
    return null;
  }

  if (!pluginReady || data === null) return null;

  const RendererComponent = getPluginRenderer(mime);
  if (!RendererComponent) {
    if (plainFallback) return <AnsiText text={plainFallback} />;
    return null;
  }

  return <RendererComponent data={data} mimeType={mime} />;
}

function FetchAndRender({ mime, raw, plainFallback }: { mime: string; raw: string; plainFallback?: string }) {
  const [content, setContent] = useState<string | null>(
    isBlobUrl(raw) ? null : raw,
  );
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    if (isBlobUrl(raw)) {
      fetchBlobText(raw)
        .then(setContent)
        .catch(() => setFailed(true));
    }
  }, [raw]);

  if (failed) {
    if (plainFallback) return <AnsiText text={plainFallback} />;
    return null;
  }

  if (content === null) return null;

  switch (mime) {
    case "text/html":
      return <HtmlOutput html={content} />;
    case "image/svg+xml":
      return <HtmlOutput html={content} />;
    case "application/json":
      return <JsonOutput data={content} />;
    case "text/plain":
      return <AnsiText text={content} />;
    default:
      return <AnsiText text={content} />;
  }
}

export function StreamOutput({ output }: { output: CellOutput }) {
  const [text, setText] = useState<string | null>(null);

  useEffect(() => {
    const raw = output.text || "";
    if (isBlobUrl(raw)) {
      fetchBlobText(raw).then(setText).catch(() => setText(raw));
    } else {
      setText(raw);
    }
  }, [output.text]);

  if (!text) return null;

  const className = output.name === "stderr" ? "stream stream-stderr" : "stream";
  return <AnsiText text={text} className={className} />;
}
