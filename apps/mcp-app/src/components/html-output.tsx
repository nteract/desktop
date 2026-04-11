import { useEffect, useRef } from "react";

interface HtmlOutputProps {
  html: string;
}

export function HtmlOutput({ html }: HtmlOutputProps) {
  const frameRef = useRef<HTMLIFrameElement>(null);

  useEffect(() => {
    const frame = frameRef.current;
    if (!frame) return;
    const handleLoad = () => {
      try {
        const h = frame.contentDocument?.documentElement?.scrollHeight;
        if (h) frame.style.height = `${h + 2}px`;
      } catch {
        /* cross-origin */
      }
    };
    frame.addEventListener("load", handleLoad);
    return () => frame.removeEventListener("load", handleLoad);
  }, []);

  const styles =
    typeof document !== "undefined" ? getComputedStyle(document.documentElement) : null;
  const bg = styles?.getPropertyValue("--code-bg").trim() || "#1e1e1e";
  const fg = styles?.getPropertyValue("--fg").trim() || "#e5e5e5";
  const border = styles?.getPropertyValue("--border").trim() || "#374151";
  const codeBg = styles?.getPropertyValue("--code-bg").trim() || "#262626";
  const fgMuted = styles?.getPropertyValue("--fg-muted").trim() || "#9ca3af";

  const srcdoc = `<!DOCTYPE html><html><head><style>
    * { box-sizing: border-box; margin: 0; padding: 0; }
    body { font-family: system-ui, -apple-system, sans-serif; color: ${fg}; background: ${bg}; }
    table { border-collapse: collapse; font-family: ui-monospace, SFMono-Regular, monospace; font-size: 13px; }
    th, td { padding: 6px 10px; border: 1px solid ${border}; text-align: left; }
    th { background: ${codeBg}; color: ${fgMuted}; font-weight: 600; }
    tr:hover td { background: ${codeBg}; }
  </style></head><body>${html}</body></html>`;

  return (
    <iframe
      ref={frameRef}
      className="html-output"
      sandbox="allow-scripts"
      srcDoc={srcdoc}
      title="HTML output"
    />
  );
}
