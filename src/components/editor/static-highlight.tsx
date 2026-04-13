import type { ReactNode } from "react";
import { HighlightStyle } from "@codemirror/language";
import type { Parser } from "@lezer/common";
import { highlightCode } from "@lezer/highlight";

import { pythonLanguage } from "@codemirror/lang-python";
import { javascriptLanguage, tsxLanguage, jsxLanguage } from "@codemirror/lang-javascript";
import { jsonLanguage } from "@codemirror/lang-json";
import { yamlLanguage } from "@codemirror/lang-yaml";
import { htmlLanguage } from "@codemirror/lang-html";
import { StandardSQL } from "@codemirror/lang-sql";
import { markdownLanguage } from "@codemirror/lang-markdown";

import {
  classicLightStyle,
  classicDarkStyle,
  creamLightStyle,
  creamDarkStyle,
} from "./highlight-styles";
import type { ColorTheme } from "./themes";

export type { ColorTheme };

// ---------------------------------------------------------------------------
// Language parser map
// ---------------------------------------------------------------------------

const languageParsers: Record<string, Parser> = {
  python: pythonLanguage.parser,
  py: pythonLanguage.parser,
  javascript: javascriptLanguage.parser,
  js: javascriptLanguage.parser,
  typescript: tsxLanguage.parser,
  ts: tsxLanguage.parser,
  jsx: jsxLanguage.parser,
  tsx: tsxLanguage.parser,
  json: jsonLanguage.parser,
  yaml: yamlLanguage.parser,
  yml: yamlLanguage.parser,
  html: htmlLanguage.parser,
  sql: StandardSQL.language.parser,
  markdown: markdownLanguage.parser,
  md: markdownLanguage.parser,
};

/**
 * List of supported language identifiers for static highlighting.
 */
export const supportedLanguages = Object.keys(languageParsers);

// ---------------------------------------------------------------------------
// Highlight style resolution
// ---------------------------------------------------------------------------

/**
 * Get the appropriate HighlightStyle for a given theme variant.
 */
function getHighlightStyle(isDark: boolean, colorTheme: ColorTheme): HighlightStyle {
  if (colorTheme === "cream") {
    return HighlightStyle.define(isDark ? creamDarkStyle : creamLightStyle);
  }
  return HighlightStyle.define(isDark ? classicDarkStyle : classicLightStyle);
}

// ---------------------------------------------------------------------------
// Core highlight function
// ---------------------------------------------------------------------------

/**
 * Highlight code into an array of React nodes using CodeMirror's Lezer
 * parsers and our owned highlight styles.
 *
 * Returns plain text if the language is not recognized.
 */
function highlight(
  code: string,
  language: string | undefined,
  isDark: boolean,
  colorTheme: ColorTheme,
): ReactNode[] {
  const parser = language ? languageParsers[language.toLowerCase()] : undefined;
  if (!parser) {
    return [code];
  }

  const tree = parser.parse(code);
  const style = getHighlightStyle(isDark, colorTheme);
  const nodes: ReactNode[] = [];
  let key = 0;

  highlightCode(
    code,
    tree,
    style,
    (text: string, classes: string) => {
      if (classes) {
        nodes.push(
          <span key={key++} className={classes}>
            {text}
          </span>,
        );
      } else {
        nodes.push(text);
        key++;
      }
    },
    () => {
      nodes.push("\n");
      key++;
    },
  );

  return nodes;
}

// ---------------------------------------------------------------------------
// Theme colors for static blocks
// ---------------------------------------------------------------------------

interface BlockColors {
  background: string;
  foreground: string;
}

function getBlockColors(isDark: boolean, colorTheme: ColorTheme): BlockColors {
  if (colorTheme === "cream") {
    return isDark
      ? { background: "#1a1816", foreground: "#ebdbb2" }
      : { background: "#f0ede7", foreground: "#3c3836" };
  }
  return isDark
    ? { background: "#161b22", foreground: "#c9d1d9" }
    : { background: "#f6f8fa", foreground: "#24292f" };
}

// ---------------------------------------------------------------------------
// StaticCodeBlock component
// ---------------------------------------------------------------------------

interface StaticCodeBlockProps {
  code: string;
  language?: string;
  isDark?: boolean;
  colorTheme?: ColorTheme;
  className?: string;
}

/**
 * Renders syntax-highlighted code in a `<pre>` block without requiring a
 * CodeMirror editor instance. Uses Lezer parsers and our owned highlight
 * styles for consistent coloring.
 */
export function StaticCodeBlock({
  code,
  language,
  isDark = false,
  colorTheme = "classic",
  className,
}: StaticCodeBlockProps) {
  const colors = getBlockColors(isDark, colorTheme);
  const nodes = highlight(code, language, isDark, colorTheme);

  return (
    <pre
      className={className}
      style={{
        backgroundColor: colors.background,
        color: colors.foreground,
        padding: "12px 16px",
        fontFamily:
          'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace',
        fontSize: "13px",
        lineHeight: 1.5,
        borderRadius: "6px",
        margin: 0,
        overflow: "auto",
        whiteSpace: "pre",
      }}
    >
      <code>{nodes}</code>
    </pre>
  );
}
