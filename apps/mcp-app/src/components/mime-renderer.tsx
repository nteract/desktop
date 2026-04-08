import { useEffect, useRef, useState } from "react";
import { selectMimeType } from "../lib/mime-priority";
import { fetchBlobText, isBlobUrl } from "../lib/blob-fetch";
import { AnsiText } from "./ansi-text";
import { ImageOutput } from "./image-output";
import { HtmlOutput } from "./html-output";
import { MarkdownOutput } from "./markdown-output";
import { JsonOutput } from "./json-output";
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
}

export function MimeRenderer({ data }: MimeRendererProps) {
  const mime = selectMimeType(data);
  if (!mime) return null;
  const raw = data[mime];
  if (raw == null) return null;

  // Images: use blob URL directly as <img src>, no fetch needed
  if (mime.startsWith("image/") && mime !== "image/svg+xml") {
    return <ImageOutput data={String(raw)} mediaType={mime} alt={data["text/plain"] || undefined} />;
  }

  // text/plain fallback for when blob fetch fails
  const plainFallback = data["text/plain"] ? String(data["text/plain"]) : undefined;

  return <FetchAndRender mime={mime} raw={String(raw)} plainFallback={plainFallback} />;
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
