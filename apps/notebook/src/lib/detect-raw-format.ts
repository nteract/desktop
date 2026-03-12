import type { CellMetadata } from "../types";

export type RawFormat = "yaml" | "toml" | "json" | "plain";

/**
 * Detect the format of raw cell content for syntax highlighting.
 *
 * Priority:
 * 1. Cell metadata.format if explicitly set
 * 2. Content heuristics (frontmatter delimiters, section headers, etc.)
 * 3. Fallback to plain text
 */
export function detectRawFormat(
  source: string,
  metadata?: CellMetadata,
): RawFormat {
  // 1. Check metadata.format (explicit user/API setting)
  const metaFormat = metadata?.format;
  if (typeof metaFormat === "string") {
    const lower = metaFormat.toLowerCase();
    if (lower === "yaml" || lower === "yml") return "yaml";
    if (lower === "toml") return "toml";
    if (lower === "json") return "json";
  }

  // 2. Content heuristics
  const trimmed = source.trim();
  if (!trimmed) return "plain";

  // YAML frontmatter: starts with ---
  if (trimmed.startsWith("---")) return "yaml";

  // TOML: has [section] headers (check before JSON to avoid [section] matching JSON array)
  // Section headers look like [package] or [tool.poetry], not [" or [1
  if (/^\[[\w.-]+\]\s*$/m.test(trimmed)) return "toml";

  // JSON: starts with { or [ followed by typical JSON content
  if (/^{/.test(trimmed)) return "json";
  if (/^\[[\s\n]*["\d[{]/.test(trimmed)) return "json";

  // TOML: key = value patterns
  if (/^\w[\w-]*\s*=\s*/m.test(trimmed)) return "toml";

  // YAML: contains key: value patterns (but not URLs like http:)
  if (/^[a-zA-Z_][\w-]*:\s/m.test(trimmed)) return "yaml";

  return "plain";
}
