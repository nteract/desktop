import { html } from "@codemirror/lang-html";
import { javascript } from "@codemirror/lang-javascript";
import { json } from "@codemirror/lang-json";
import { markdown } from "@codemirror/lang-markdown";
import { python } from "@codemirror/lang-python";
import { sql } from "@codemirror/lang-sql";
import { yaml } from "@codemirror/lang-yaml";
import { indentUnit, LanguageSupport, LRLanguage } from "@codemirror/language";
import type { Extension } from "@codemirror/state";
import { parser as tomlParser } from "lezer-toml";

import {
  CELL_MAGIC_LANGUAGES,
  detectCellMagic,
  getCellMagicLanguage,
  ipythonHighlighting,
  ipythonStyles,
  ipythonStylesDark,
} from "./ipython";

/**
 * Supported languages for the CodeMirror editor
 */
export type SupportedLanguage =
  | "python"
  | "ipython"
  | "markdown"
  | "sql"
  | "html"
  | "javascript"
  | "typescript"
  | "json"
  | "yaml"
  | "toml"
  | "plain";

/**
 * Get the CodeMirror language extension for a given language
 */
// PEP 8 specifies 4-space indentation for Python
const pythonIndent = indentUnit.of("    ");

// TOML language support using lezer-toml
const tomlLanguage = LRLanguage.define({
  languageData: {
    commentTokens: { line: "#" },
  },
  name: "toml",
  parser: tomlParser,
});

function toml() {
  return new LanguageSupport(tomlLanguage);
}

export function getLanguageExtension(language: SupportedLanguage): Extension {
  switch (language) {
    case "python":
      return [python(), pythonIndent];
    case "ipython":
      return [python(), pythonIndent, ipythonHighlighting(), ipythonStyles, ipythonStylesDark];
    case "markdown":
      return markdown();
    case "sql":
      return sql();
    case "html":
      return html();
    case "javascript":
      return javascript();
    case "typescript":
      return javascript({ typescript: true });
    case "json":
      return json();
    case "yaml":
      return yaml();
    case "toml":
      return toml();
    default:
      return [];
  }
}

/**
 * Get the language extension for IPython content, detecting cell magics.
 *
 * If the content starts with a cell magic (e.g., %%html, %%bash),
 * returns the appropriate language extension for that magic.
 * Otherwise returns the standard IPython extension.
 *
 * @param content - The editor content to analyze
 * @returns Object with language extension and detected cell magic (if any)
 *
 * @example
 * // Cell magic - returns HTML language
 * getIPythonExtension("%%html\n<div>Hello</div>")
 * // { extension: html(), cellMagic: "html", language: "html" }
 *
 * // No cell magic - returns IPython
 * getIPythonExtension("%time x = sum(range(100))")
 * // { extension: [python(), ...], cellMagic: null, language: "ipython" }
 */
export function getIPythonExtension(content: string): {
  extension: Extension;
  cellMagic: string | null;
  language: SupportedLanguage;
} {
  const magic = detectCellMagic(content);

  if (magic) {
    const langId = getCellMagicLanguage(magic);
    const language = (langId in languageDisplayNames ? langId : "plain") as SupportedLanguage;

    // For cell magics, use the target language but add IPython decoration
    // for the first line (the %%magic declaration)
    const baseExtension = getLanguageExtension(language);
    return {
      extension: [baseExtension, ipythonHighlighting(), ipythonStyles, ipythonStylesDark],
      cellMagic: magic,
      language,
    };
  }

  // No cell magic - use standard IPython mode
  return {
    extension: getLanguageExtension("ipython"),
    cellMagic: null,
    language: "ipython",
  };
}

// Re-export cell magic utilities for consumers
export { CELL_MAGIC_LANGUAGES, detectCellMagic, getCellMagicLanguage };

/**
 * Language display names for UI
 */
export const languageDisplayNames: Record<SupportedLanguage, string> = {
  python: "Python",
  ipython: "IPython",
  markdown: "Markdown",
  sql: "SQL",
  html: "HTML",
  javascript: "JavaScript",
  typescript: "TypeScript",
  json: "JSON",
  yaml: "YAML",
  toml: "TOML",
  plain: "Plain Text",
};

/**
 * File extensions mapped to languages
 */
export const fileExtensionToLanguage: Record<string, SupportedLanguage> = {
  ".py": "python",
  ".ipy": "ipython",
  ".md": "markdown",
  ".markdown": "markdown",
  ".sql": "sql",
  ".html": "html",
  ".htm": "html",
  ".js": "javascript",
  ".jsx": "javascript",
  ".ts": "typescript",
  ".tsx": "typescript",
  ".json": "json",
  ".yaml": "yaml",
  ".yml": "yaml",
  ".toml": "toml",
  ".txt": "plain",
};

/**
 * Detect language from filename
 */
export function detectLanguage(filename: string): SupportedLanguage {
  const ext = filename.slice(filename.lastIndexOf(".")).toLowerCase();
  return fileExtensionToLanguage[ext] || "plain";
}
