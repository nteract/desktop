import { useEffect, useRef, useState } from "react";
import { fetchBlobText, isBlobUrl } from "../lib/blob-fetch";
import { selectMimeType } from "../lib/mime-priority";
import { loadPluginForMime, needsDaemonPlugin } from "../lib/plugin-loader";
import { getPluginRenderer } from "../lib/plugin-executor";
import { AnsiText } from "./ansi-text";
import { HtmlOutput } from "./html-output";
import { ImageOutput } from "./image-output";
import { JsonOutput } from "./json-output";
import { MarkdownOutput } from "./markdown-output";
import type { CellOutput } from "../types";
import katex from "katex";

function LatexOutput({ content }: { content: string }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!ref.current || !content.trim()) return;
    let latex = content.trim();
    if (latex.startsWith("$$") && latex.endsWith("$$")) {
      latex = latex.slice(2, -2).trim();
    } else if (latex.startsWith("$") && latex.endsWith("$")) {
      latex = latex.slice(1, -1).trim();
    }
    katex.render(latex, ref.current, {
      displayMode: true,
      throwOnError: false,
      trust: true,
    });
  }, [content]);
  return <div className="latex-output" ref={ref} />;
}

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

  // Viz MIME types: need a daemon-served plugin to render
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
 * Load a daemon-served plugin via <script> tag and render viz data.
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

    // Load plugin via <script> tag and fetch blob data in parallel
    const pluginPromise = loadPluginForMime(mime, blobBaseUrl) ?? Promise.resolve();
    const dataPromise = isBlobUrl(raw)
      ? fetchBlobText(raw).then((text) => JSON.parse(text))
      : Promise.resolve(typeof raw === "string" ? JSON.parse(raw) : raw);

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

  // Look up the registered renderer component
  const RendererComponent = getPluginRenderer(mime);
  if (!RendererComponent) {
    // Plugin loaded but didn't register for this MIME type
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

  // Blob fetch failed — show text/plain fallback if available
  if (failed) {
    if (plainFallback) return <AnsiText text={plainFallback} />;
    return null;
  }

  if (content === null) return null;

  switch (mime) {
    case "text/html":
      return <HtmlOutput html={content} />;
    case "text/markdown":
      return <MarkdownOutput content={content} />;
    case "text/latex":
      return <LatexOutput content={content} />;
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
