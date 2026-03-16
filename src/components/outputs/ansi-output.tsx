import Anser from "anser";
import { escapeCarriageReturn } from "escape-carriage";
import type { CSSProperties, ReactNode } from "react";
import { cn } from "@/lib/utils";

/**
 * Theme-aware ANSI color mapping.
 *
 * We call anser's ansiToJson with use_classes: true so it gives us structured
 * data we can remap. The base 16 ANSI colors go through CSS variables so they
 * adapt to light/dark mode. Extended colors (256-color palette and 24-bit
 * truecolor) render as inline rgb() styles since they're already precise.
 *
 * anser returns these shapes in class mode:
 *
 *   Standard 16:    fg = "ansi-red",           fg_truecolor = null
 *   256 (0-15):     fg = "ansi-red" etc,       fg_truecolor = null
 *   256 (16-255):   fg = "ansi-palette-123",   fg_truecolor = null
 *   24-bit RGB:     fg = "ansi-truecolor",     fg_truecolor = "237, 17, 128"
 *   No color:       fg = null,                 fg_truecolor = null
 */

// The 16 class names anser emits for standard colors.
const ANSI_CLASS_NAMES = new Set([
  "ansi-black",
  "ansi-red",
  "ansi-green",
  "ansi-yellow",
  "ansi-blue",
  "ansi-magenta",
  "ansi-cyan",
  "ansi-white",
  "ansi-bright-black",
  "ansi-bright-red",
  "ansi-bright-green",
  "ansi-bright-yellow",
  "ansi-bright-blue",
  "ansi-bright-magenta",
  "ansi-bright-cyan",
  "ansi-bright-white",
]);

function isStandardColor(name: string | null): boolean {
  return name !== null && ANSI_CLASS_NAMES.has(name);
}

function isPaletteColor(name: string | null): boolean {
  return !!name?.startsWith("ansi-palette-");
}

/**
 * Resolve a 256-color palette index to an rgb() string.
 *
 * Indices 0-15 are handled by anser as standard class names.
 * Indices 16-231 are a 6×6×6 color cube.
 * Indices 232-255 are a grayscale ramp.
 */
function paletteIndexToRgb(index: number): string {
  if (index >= 16 && index <= 231) {
    const adjusted = index - 16;
    const r = Math.floor(adjusted / 36);
    const g = Math.floor((adjusted % 36) / 6);
    const b = adjusted % 6;
    return `rgb(${r ? r * 40 + 55 : 0}, ${g ? g * 40 + 55 : 0}, ${b ? b * 40 + 55 : 0})`;
  }
  if (index >= 232 && index <= 255) {
    const level = (index - 232) * 10 + 8;
    return `rgb(${level}, ${level}, ${level})`;
  }
  return "inherit";
}

/**
 * Parse an anser JSON entry's color fields into a React style + className.
 */
function resolveAnsiStyle(entry: Anser.AnserJsonEntry): {
  style: CSSProperties;
  className: string;
} {
  const style: CSSProperties = {};
  const classes: string[] = [];

  // Foreground
  if (entry.fg) {
    if (isStandardColor(entry.fg)) {
      // Use CSS variable via class: .ansi-red-fg { color: var(--ansi-red) }
      classes.push(`${entry.fg}-fg`);
    } else if (entry.fg === "ansi-truecolor" && entry.fg_truecolor) {
      style.color = `rgb(${entry.fg_truecolor})`;
    } else if (isPaletteColor(entry.fg)) {
      const index = parseInt(entry.fg.replace("ansi-palette-", ""), 10);
      style.color = paletteIndexToRgb(index);
    }
  }

  // Background
  if (entry.bg) {
    if (isStandardColor(entry.bg)) {
      classes.push(`${entry.bg}-bg`);
    } else if (entry.bg === "ansi-truecolor" && entry.bg_truecolor) {
      style.backgroundColor = `rgb(${entry.bg_truecolor})`;
    } else if (isPaletteColor(entry.bg)) {
      const index = parseInt(entry.bg.replace("ansi-palette-", ""), 10);
      style.backgroundColor = paletteIndexToRgb(index);
    }
  }

  // Decorations
  for (const decoration of entry.decorations) {
    switch (decoration) {
      case "bold":
        style.fontWeight = "bold";
        break;
      case "dim":
        style.opacity = 0.5;
        break;
      case "italic":
        style.fontStyle = "italic";
        break;
      case "hidden":
        style.visibility = "hidden";
        break;
      case "strikethrough":
        style.textDecoration =
          style.textDecoration === "underline"
            ? "underline line-through"
            : "line-through";
        break;
      case "underline":
        style.textDecoration =
          style.textDecoration === "line-through"
            ? "underline line-through"
            : "underline";
        break;
    }
  }

  return { style, className: classes.join(" ") };
}

/**
 * Backspace handling ported from ansi-to-react (originally from Jupyter Classic).
 */
function fixBackspace(txt: string): string {
  let result = txt;
  let previous: string;
  do {
    previous = result;
    // biome-ignore lint/suspicious/noControlCharactersInRegex: intentional backspace (\x08) matching for terminal emulation
    result = result.replace(/[^\n]\x08/gm, "");
  } while (result.length < previous.length);
  return result;
}

/**
 * Parse ANSI text into structured JSON entries using anser.
 */
function ansiToJson(input: string): Anser.AnserJsonEntry[] {
  const cleaned = escapeCarriageReturn(fixBackspace(input));
  return Anser.ansiToJson(cleaned, {
    json: true,
    remove_empty: true,
    use_classes: true,
  });
}

/**
 * Render parsed ANSI entries to React spans.
 */
function renderAnsiEntries(entries: Anser.AnserJsonEntry[]): ReactNode[] {
  return entries.map((entry, i) => {
    const { style, className } = resolveAnsiStyle(entry);
    const hasStyle = Object.keys(style).length > 0;
    const hasClass = className.length > 0;

    if (!hasStyle && !hasClass) {
      return <span key={i}>{entry.content}</span>;
    }

    return (
      <span
        key={i}
        style={hasStyle ? style : undefined}
        className={hasClass ? className : undefined}
      >
        {entry.content}
      </span>
    );
  });
}

// ---------------------------------------------------------------------------
// Public components
// ---------------------------------------------------------------------------

interface AnsiOutputProps {
  children: string;
  className?: string;
  isError?: boolean;
}

/**
 * AnsiOutput renders ANSI escape sequences as colored text.
 *
 * Standard 16 colors use CSS variables (theme-aware, adapts to light/dark).
 * 256-color and 24-bit truecolor use inline rgb() styles for full fidelity.
 */
export function AnsiOutput({
  children,
  className = "",
  isError = false,
}: AnsiOutputProps) {
  if (!children || typeof children !== "string") {
    return null;
  }

  const entries = ansiToJson(children);

  return (
    <div
      data-slot="ansi-output"
      className={cn(
        "not-prose font-mono text-sm whitespace-pre-wrap leading-relaxed",
        isError && "text-red-600 dark:text-red-400",
        className,
      )}
    >
      <code>{renderAnsiEntries(entries)}</code>
    </div>
  );
}

interface AnsiStreamOutputProps {
  text: string;
  streamName: "stdout" | "stderr";
  className?: string;
}

/**
 * AnsiStreamOutput component specifically for stdout/stderr rendering.
 */
export function AnsiStreamOutput({
  text,
  streamName,
  className = "",
}: AnsiStreamOutputProps) {
  const isStderr = streamName === "stderr";
  const streamClasses = isStderr
    ? "text-red-600 dark:text-red-400"
    : "text-gray-700 dark:text-gray-300";

  return (
    <div
      data-slot="ansi-stream-output"
      className={cn("not-prose py-2", streamClasses, className)}
    >
      <AnsiOutput isError={isStderr}>{text}</AnsiOutput>
    </div>
  );
}

interface AnsiErrorOutputProps {
  ename?: string;
  evalue?: string;
  traceback?: string[] | string;
  className?: string;
}

/**
 * AnsiErrorOutput component specifically for error messages and tracebacks.
 */
export function AnsiErrorOutput({
  ename,
  evalue,
  traceback,
  className = "",
}: AnsiErrorOutputProps) {
  return (
    <div
      data-slot="ansi-error-output"
      className={cn(
        "not-prose py-3",
        className,
      )}
    >
      {ename && evalue && (
        <div className="mb-1 font-semibold text-red-700 dark:text-red-400">
          <AnsiOutput isError>{`${ename}: ${evalue}`}</AnsiOutput>
        </div>
      )}
      {traceback && (
        <div className="mt-2 text-xs text-red-600 dark:text-red-400 opacity-80">
          <AnsiOutput isError>
            {Array.isArray(traceback) ? traceback.join("\n") : traceback}
          </AnsiOutput>
        </div>
      )}
    </div>
  );
}
