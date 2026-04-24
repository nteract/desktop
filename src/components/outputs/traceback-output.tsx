/**
 * Rich traceback renderer — experimental.
 *
 * Consumes `application/vnd.nteract.traceback+json` payloads. Schema is
 * deliberately loose while we iterate from a notebook; anything we don't
 * recognize renders via a "raw JSON" escape hatch so we can't lock
 * ourselves out of debugging the payload shape.
 *
 * Live iteration loop: emit a `display_data` message from a notebook cell
 * with this MIME and hack on the component. No kernel-side dependency,
 * no persistence, no plugin build step — main-DOM React all the way.
 */

import { Check, ChevronRight, Copy, OctagonAlert } from "lucide-react";
import { useState } from "react";
import { highlight } from "@/components/editor/static-highlight";
import { useColorTheme, useDarkMode } from "@/lib/dark-mode";
import { cn } from "@/lib/utils";

/** CodeMirror-matched mono stack so the traceback reads like the editor. */
const CM_FONT_FAMILY =
  'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace';

/** Source-context line shown around the failing line in a frame. */
interface Line {
  lineno: number;
  source: string;
  /** True for the line that actually raised. One per frame. */
  highlight?: boolean;
}

/** A single frame in the call stack. */
interface Frame {
  /** Absolute or relative path of the file the frame lives in. */
  filename: string;
  /** Line number of the failing call. */
  lineno: number;
  /** Enclosing function / method / module name. */
  name: string;
  /** Optional source-context window — lines around `lineno`. */
  lines?: Line[];
  /** Optional "in library code" flag; lets the UI dim non-user frames. */
  library?: boolean;
}

/**
 * Parse-error-only info, populated when the exception is a
 * `SyntaxError` / `IndentationError` / `TabError`. These don't have
 * user-code frames (IPython raises from `ast_parse` before any cell
 * bytecode runs), so we dedicate a slot to what actually helps:
 * the offending source line and a caret at `offset`.
 */
interface SyntaxInfo {
  filename: string;
  lineno: number;
  offset: number;
  text: string;
  msg: string;
}

interface TracebackPayload {
  /** Exception class name, e.g. "ValueError". */
  ename: string;
  /** Exception message. */
  evalue: string;
  /** Frames, outermost first (Python convention). */
  frames?: Frame[];
  /**
   * Language for syntax highlighting of source lines. Defaults to
   * "python" since that's what we emit today; Deno notebooks will
   * ship "typescript" here. Unknown values fall back to plain text.
   */
  language?: string;
  /**
   * Paste-ready plain text version of the traceback — ANSI-stripped,
   * in the same shape the kernel would emit as `text/llm+plain`. Used
   * by the Copy button. Kernel owns this; we don't re-synthesize.
   */
  text?: string;
  /** Raw traceback strings, for cases where we couldn't parse. */
  raw?: string[];
  /**
   * Present for `SyntaxError` / `IndentationError` / `TabError`.
   * When set, the renderer shows a dedicated parse-error layout
   * (source line + caret) instead of a frame list.
   */
  syntax?: SyntaxInfo;
}

interface Props {
  data: unknown;
  className?: string;
}

/** A single frame, or a run of consecutive identical frames (recursion). */
interface Cluster {
  frame: Frame;
  /** Number of consecutive frames this represents. >1 on recursion. */
  count: number;
  /** Original index of the representative frame, for key stability. */
  firstIndex: number;
}

/**
 * Group consecutive frames sharing (filename, lineno, name) into one
 * cluster. Turns a 2978-frame RecursionError into a single row labeled
 * "× 2978" instead of tanking the DOM with thousands of `<pre>` blocks.
 */
function clusterFrames(frames: Frame[]): Cluster[] {
  const out: Cluster[] = [];
  for (let i = 0; i < frames.length; i++) {
    const f = frames[i];
    const last = out[out.length - 1];
    if (
      last &&
      last.frame.filename === f.filename &&
      last.frame.lineno === f.lineno &&
      last.frame.name === f.name
    ) {
      last.count += 1;
    } else {
      out.push({ frame: f, count: 1, firstIndex: i });
    }
  }
  return out;
}

export function TracebackOutput({ data, className }: Props) {
  const payload = toPayload(data);
  if (!payload) {
    return <RawJsonFallback data={data} className={className} />;
  }
  const frames = payload.frames ?? [];
  const clusters = clusterFrames(frames);
  const language = payload.language ?? "python";
  // Expand user frames by default, collapse library frames. Matches how
  // humans read tracebacks: their own code first, stdlib noise tucked
  // away. Two extra rules:
  //   - Recursion clusters (count > 1) stay collapsed; they're noise.
  //   - If everything is library code, open the innermost so something
  //     useful is visible.
  const everythingIsLibrary = clusters.length > 0 && clusters.every((c) => c.frame.library);
  const innermost = clusters.length - 1;
  const shouldOpen = (c: Cluster, i: number): boolean => {
    if (c.count > 1) return false;
    if (everythingIsLibrary) return i === innermost;
    return !c.frame.library;
  };

  // Single-frame polish: when there's exactly one user frame named
  // `<module>` (cell top-level), inline the evalue next to the ename
  // in the header and skip the chevron row. Most real cell errors are
  // this shape; the ceremonial per-frame row is just noise.
  const collapseSingleFrame =
    clusters.length === 1 && clusters[0].frame.name === "<module>" && !clusters[0].frame.library;

  return (
    <div
      data-slot="traceback"
      className={cn(
        "rounded-md border border-destructive/25 bg-destructive/5",
        "text-sm",
        className,
      )}
    >
      <Header
        ename={payload.ename}
        evalue={payload.evalue}
        payload={payload}
        inlineEvalue={collapseSingleFrame || Boolean(payload.syntax)}
      />
      {payload.syntax ? (
        <SyntaxErrorBlock syntax={payload.syntax} language={language} />
      ) : collapseSingleFrame && clusters[0].frame.lines && clusters[0].frame.lines.length > 0 ? (
        <div className="px-3 pb-2">
          <SourceBlock lines={clusters[0].frame.lines} language={language} />
        </div>
      ) : (
        clusters.length > 0 && (
          <ol className="divide-y divide-destructive/15">
            {clusters.map((cluster, i) => (
              <FrameRow
                key={`${cluster.frame.filename}:${cluster.frame.lineno}:${cluster.firstIndex}`}
                cluster={cluster}
                defaultOpen={shouldOpen(cluster, i)}
                language={language}
              />
            ))}
          </ol>
        )
      )}
    </div>
  );
}

// ─── Pieces ────────────────────────────────────────────────────────────

function Header({
  ename,
  evalue,
  payload,
  inlineEvalue,
}: {
  ename: string;
  evalue: string;
  payload: TracebackPayload;
  /**
   * When true, render `ename: evalue` on a single line (for the
   * single-frame and SyntaxError cases where the two-line header
   * is unnecessary ceremony).
   */
  inlineEvalue?: boolean;
}) {
  // Multi-line evalues (pytest AssertionError, chained SQL errors)
  // never inline cleanly — fall through to the two-line layout even
  // when the caller asked for inline.
  const canInline = inlineEvalue && evalue && !evalue.includes("\n");
  return (
    <div className="flex items-start gap-2 px-3 py-2 font-mono">
      <OctagonAlert
        aria-hidden="true"
        className="mt-0.5 h-4 w-4 shrink-0 text-destructive"
        strokeWidth={2.5}
      />
      <div className="min-w-0 flex-1">
        {canInline ? (
          <div>
            <span className="font-semibold text-destructive">{ename}</span>
            <span className="text-destructive/80">: </span>
            <span className="whitespace-pre-wrap break-words text-foreground/85">{evalue}</span>
          </div>
        ) : (
          <>
            <div className="font-semibold text-destructive">{ename}</div>
            {evalue && (
              <div className="mt-0.5 whitespace-pre-wrap break-words text-foreground/85">
                {evalue}
              </div>
            )}
          </>
        )}
      </div>
      <CopyButton payload={payload} />
    </div>
  );
}

/**
 * Parse-error layout. Shows the offending source line with a caret at
 * the `offset` column. No frame list (SyntaxError doesn't have one
 * that'd be useful). If `text` is empty (older Python versions don't
 * always populate it), we show just the header.
 */
function SyntaxErrorBlock({ syntax, language }: { syntax: SyntaxInfo; language: string }) {
  const isDark = useDarkMode();
  const rawTheme = useColorTheme();
  const colorTheme = rawTheme === "cream" ? "cream" : "classic";

  if (!syntax.text) {
    return null;
  }

  // `offset` is 1-based (CPython convention). Clamp to the line length
  // so caret padding never blows past the content.
  const caretCol = Math.max(1, Math.min(syntax.offset || 1, syntax.text.length + 1));
  const caretPadding = " ".repeat(caretCol - 1);

  const gutterWidth = String(syntax.lineno || 1).length;

  return (
    <pre
      className={cn(
        "mx-3 mb-2 overflow-x-auto rounded border border-destructive/15 bg-muted/40",
        "px-2 py-1.5 leading-5",
      )}
      style={{ fontFamily: CM_FONT_FAMILY, fontSize: "13px" }}
    >
      <div className="grid grid-cols-[auto_1fr] gap-x-3 border-l-2 border-destructive bg-destructive/10 pl-1.5">
        <span
          className="select-none text-right font-semibold tabular-nums text-destructive"
          style={{ minWidth: `${gutterWidth}ch` }}
        >
          ▸{String(syntax.lineno || 1).padStart(gutterWidth, " ")}
        </span>
        <code className="whitespace-pre">
          {highlight(syntax.text, language, isDark, colorTheme)}
        </code>
      </div>
      <div className="grid grid-cols-[auto_1fr] gap-x-3 border-l-2 border-transparent pl-1.5">
        <span
          className="select-none text-right tabular-nums text-muted-foreground/50"
          style={{ minWidth: `${gutterWidth}ch` }}
        >
          {" ".repeat(gutterWidth + 1)}
        </span>
        <code className="whitespace-pre text-destructive">
          {caretPadding}
          <span aria-hidden="true">^</span>
        </code>
      </div>
    </pre>
  );
}

function CopyButton({ payload }: { payload: TracebackPayload }) {
  const [copied, setCopied] = useState(false);

  const onClick = async () => {
    // Paste-ready text comes from the payload. The kernel already has
    // the ANSI-stripped traceback in the shape every AI and search
    // engine expects — re-synthesizing it here would just drift.
    const text =
      payload.text ??
      (payload.raw && payload.raw.length > 0
        ? `${payload.raw.join("\n")}\n${payload.ename}: ${payload.evalue}`
        : `${payload.ename}: ${payload.evalue}`);
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch (err) {
      // Clipboard can fail in some iframe/permission contexts. Fall back
      // silently — users can still select the rendered text by hand.
      console.warn("[traceback] copy failed:", err);
    }
  };

  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={copied ? "Copied" : "Copy traceback"}
      title={copied ? "Copied" : "Copy traceback"}
      className={cn(
        "shrink-0 rounded px-1.5 py-1 text-xs font-mono transition-colors",
        "text-muted-foreground hover:bg-destructive/10 hover:text-destructive",
      )}
    >
      <span className="flex items-center gap-1">
        {copied ? (
          <Check aria-hidden="true" className="h-3.5 w-3.5 text-green-600 dark:text-green-400" />
        ) : (
          <Copy aria-hidden="true" className="h-3.5 w-3.5" />
        )}
        <span className="hidden sm:inline">{copied ? "Copied" : "Copy"}</span>
      </span>
    </button>
  );
}

function FrameRow({
  cluster,
  defaultOpen,
  language,
}: {
  cluster: Cluster;
  defaultOpen: boolean;
  language: string;
}) {
  const [open, setOpen] = useState(defaultOpen);
  const { frame, count } = cluster;
  return (
    <li className={cn("px-3 py-1.5", frame.library && "opacity-60")}>
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-2 text-left font-mono text-xs hover:text-destructive"
        aria-expanded={open}
      >
        <ChevronRight
          aria-hidden="true"
          className={cn(
            "h-3 w-3 shrink-0 text-muted-foreground transition-transform",
            open && "rotate-90",
          )}
        />
        <span className="truncate text-muted-foreground">{frame.filename}</span>
        <span className="text-muted-foreground">:</span>
        <span className="tabular-nums text-muted-foreground">{frame.lineno}</span>
        <span className="text-muted-foreground">in</span>
        <span className="truncate">{frame.name}</span>
        {count > 1 && (
          <span
            className={cn(
              "ml-auto shrink-0 rounded px-1.5 py-0.5 text-[10px] tabular-nums",
              "bg-destructive/15 text-destructive",
            )}
            title={`${count} consecutive identical frames (collapsed)`}
          >
            ×{count.toLocaleString()}
          </span>
        )}
      </button>
      {open && frame.lines && frame.lines.length > 0 && (
        <SourceBlock lines={frame.lines} language={language} />
      )}
    </li>
  );
}

function SourceBlock({ lines, language }: { lines: Line[]; language: string }) {
  const isDark = useDarkMode();
  const rawTheme = useColorTheme();
  const colorTheme = rawTheme === "cream" ? "cream" : "classic";

  const gutterWidth = String(Math.max(...lines.map((l) => l.lineno))).length;

  return (
    <pre
      className={cn(
        "mt-1.5 overflow-x-auto rounded border border-destructive/15 bg-muted/40",
        "px-2 py-1.5 leading-5",
      )}
      style={{ fontFamily: CM_FONT_FAMILY, fontSize: "13px" }}
    >
      {lines.map((line, i) => (
        <div
          key={i}
          className={cn(
            "grid grid-cols-[auto_1fr] gap-x-3 border-l-2 border-transparent pl-1.5",
            line.highlight && "border-destructive bg-destructive/10",
          )}
        >
          <span
            className={cn(
              "select-none text-right tabular-nums text-muted-foreground/70",
              line.highlight && "font-semibold text-destructive",
            )}
            style={{ minWidth: `${gutterWidth}ch` }}
          >
            {line.highlight ? "▸" : " "}
            {String(line.lineno).padStart(gutterWidth, " ")}
          </span>
          <code className="whitespace-pre">
            {highlight(line.source, language, isDark, colorTheme)}
          </code>
        </div>
      ))}
    </pre>
  );
}

function RawJsonFallback({ data, className }: { data: unknown; className?: string }) {
  return (
    <div
      className={cn(
        "rounded-md border border-yellow-300 bg-yellow-50/60 p-3 text-xs",
        "dark:border-yellow-800 dark:bg-yellow-950/30",
        className,
      )}
    >
      <div className="mb-1 font-semibold text-yellow-800 dark:text-yellow-200">
        Unparsed traceback payload
      </div>
      <pre className="overflow-x-auto whitespace-pre-wrap font-mono text-yellow-900/80 dark:text-yellow-100/80">
        {safeStringify(data)}
      </pre>
    </div>
  );
}

// ─── Payload normalization ─────────────────────────────────────────────

function toPayload(data: unknown): TracebackPayload | null {
  // Payloads arrive as either an object (already parsed) or a JSON string
  // (when a kernel sends it through paths that stringify custom +json).
  let obj: unknown = data;
  if (typeof data === "string") {
    try {
      obj = JSON.parse(data);
    } catch {
      return null;
    }
  }
  if (!obj || typeof obj !== "object") return null;
  const o = obj as Record<string, unknown>;
  if (typeof o.ename !== "string" || typeof o.evalue !== "string") {
    return null;
  }
  return {
    ename: o.ename,
    evalue: o.evalue,
    frames: Array.isArray(o.frames) ? (o.frames as Frame[]) : undefined,
    language: typeof o.language === "string" ? o.language : undefined,
    text: typeof o.text === "string" ? o.text : undefined,
    raw: Array.isArray(o.raw) ? (o.raw as string[]) : undefined,
    syntax: isSyntaxInfo(o.syntax) ? o.syntax : undefined,
  };
}

function isSyntaxInfo(v: unknown): v is SyntaxInfo {
  if (!v || typeof v !== "object") return false;
  const s = v as Record<string, unknown>;
  return (
    typeof s.filename === "string" &&
    typeof s.lineno === "number" &&
    typeof s.offset === "number" &&
    typeof s.text === "string" &&
    typeof s.msg === "string"
  );
}

function safeStringify(x: unknown): string {
  try {
    return typeof x === "string" ? x : JSON.stringify(x, null, 2);
  } catch {
    return String(x);
  }
}
