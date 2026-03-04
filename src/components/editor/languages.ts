import { html } from "@codemirror/lang-html";
import { javascript } from "@codemirror/lang-javascript";
import { json } from "@codemirror/lang-json";
import { markdown } from "@codemirror/lang-markdown";
import { python } from "@codemirror/lang-python";
import { sql } from "@codemirror/lang-sql";
import { indentUnit } from "@codemirror/language";
import type { Extension } from "@codemirror/state";

/**
 * Supported languages for the CodeMirror editor
 */
export type SupportedLanguage =
  | "python"
  | "markdown"
  | "sql"
  | "html"
  | "javascript"
  | "typescript"
  | "json"
  | "plain";

/**
 * Get the CodeMirror language extension for a given language
 */
export function getLanguageExtension(language: SupportedLanguage): Extension {
  switch (language) {
    case "python":
      return [python(), indentUnit.of("    ")];
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
    default:
      return [];
  }
}

/**
 * Language display names for UI
 */
export const languageDisplayNames: Record<SupportedLanguage, string> = {
  python: "Python",
  markdown: "Markdown",
  sql: "SQL",
  html: "HTML",
  javascript: "JavaScript",
  typescript: "TypeScript",
  json: "JSON",
  plain: "Plain Text",
};

/**
 * File extensions mapped to languages
 */
export const fileExtensionToLanguage: Record<string, SupportedLanguage> = {
  ".py": "python",
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
  ".txt": "plain",
};

/**
 * Detect language from filename
 */
export function detectLanguage(filename: string): SupportedLanguage {
  const ext = filename.slice(filename.lastIndexOf(".")).toLowerCase();
  return fileExtensionToLanguage[ext] || "plain";
}
