# CodeMirror Theme Ownership Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Own our syntax highlighting themes end-to-end — define our own CodeMirror highlight styles (classic + cream, light + dark), replace Prism with CodeMirror's static highlighting for markdown code blocks and history search, so all code in the app renders with consistent syntax colors.

**Architecture:** Replace the `@uiw/codemirror-theme-github` dependency with our own theme definitions using `HighlightStyle.define()` + `EditorView.theme()` directly. Build a static code highlighter using CodeMirror's `highlightCode()` + Lezer parsers to replace `react-syntax-highlighter`/Prism. This unifies syntax colors across code cells, markdown output code blocks, and the history search dialog.

**Tech Stack:** CodeMirror 6 (`@codemirror/language`, `@codemirror/view`, `@codemirror/state`), `@lezer/highlight`, Lezer language parsers (`@codemirror/lang-*`)

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `src/components/editor/themes.ts` | Rewrite | Theme definitions (4 themes: classic light/dark, cream light/dark) with settings + syntax styles |
| `src/components/editor/highlight-styles.ts` | Create | Shared syntax tag→color mappings, exported for reuse by static highlighter |
| `src/components/editor/static-highlight.tsx` | Create | React component that highlights code statically via Lezer parsers — replaces `SyntaxHighlighter` |
| `src/components/editor/static-highlight.test.ts` | Create | Unit tests for static highlighter |
| `src/components/outputs/markdown-output.tsx` | Modify | Replace `SyntaxHighlighter` usage with `StaticCodeBlock` |
| `src/components/outputs/syntax-highlighter.ts` | Delete | No longer needed (was Prism wrapper) |
| `apps/notebook/src/components/HistorySearchDialog.tsx` | Modify | Replace `SyntaxHighlighter` with `StaticCodeBlock` |
| `package.json` | Modify | Remove `react-syntax-highlighter`, `@types/react-syntax-highlighter`; remove `@uiw/codemirror-theme-github` |

## Color Palette Design

### Classic Light (our take on GitHub Light)

Based on GitHub's own syntax colors — we keep these familiar:

| Tag | Color | What it highlights |
|-----|-------|--------------------|
| `keyword`, `typeName`, `typeOperator` | `#cf222e` | `import`, `def`, `class`, `if`, type names |
| `variableName`, `attributeName` | `#24292f` | variable references, HTML attributes |
| `definition(variableName)` | `#8250df` | variable/function definitions |
| `propertyName` | `#8250df` | object properties, class properties |
| `number`, `bool`, `atom` | `#0550ae` | `42`, `True`, `None` |
| `string`, `regexp` | `#0a3069` | `"hello"`, `/regex/` |
| `comment` | `#6e7781` | `# comment` |
| `operator` | `#cf222e` | `+`, `=`, `->` |
| `bracket` | `#24292f` | `()`, `[]`, `{}` |
| `name`, `quote` | `#116329` | names, markdown quotes |
| `meta` | `#6e7781` | decorators, preprocessor |
| `tagName` | `#116329` | HTML/XML tags |
| `heading`, `strong` | `#24292f` | **bold**, markdown headings |
| `emphasis` | `#24292f` | *italic* |
| `link`, `url` | `#0550ae` | URLs |
| `deleted` | `#82071e` | diff deletions |
| `inserted` | `#116329` | diff insertions |
| `invalid` | `#cf222e` | syntax errors |

### Classic Dark (our take on GitHub Dark)

| Tag | Color |
|-----|-------|
| `keyword`, `typeName`, `typeOperator` | `#ff7b72` |
| `variableName`, `attributeName` | `#c9d1d9` |
| `definition(variableName)` | `#d2a8ff` |
| `propertyName` | `#d2a8ff` |
| `number`, `bool`, `atom` | `#79c0ff` |
| `string`, `regexp` | `#a5d6ff` |
| `comment` | `#8b949e` |
| `operator` | `#ff7b72` |
| `bracket` | `#c9d1d9` |
| `name`, `quote` | `#7ee787` |
| `meta` | `#8b949e` |
| `tagName` | `#7ee787` |
| `heading`, `strong` | `#d2a8ff` |
| `emphasis` | `#d2a8ff` |
| `link`, `url` | `#79c0ff` |
| `deleted` | `#ffdcd7` (bg: `#67060c`) |
| `inserted` | `#aff5b4` (bg: `#033a16`) |
| `invalid` | `#f97583` |

### Cream Light (warm-adapted syntax)

Same token mapping structure, but with warm-shifted colors that harmonize with the `#f5f2ec` background:

| Tag | Color | Notes |
|-----|-------|-------|
| `keyword`, `typeName`, `typeOperator` | `#9d0006` | Gruvbox dark red — warm keyword |
| `variableName`, `attributeName` | `#3c3836` | Warm near-black |
| `definition(variableName)` | `#8f3f71` | Gruvbox dark purple — definitions |
| `propertyName` | `#8f3f71` | Gruvbox dark purple |
| `number`, `bool`, `atom` | `#8f3f71` | Purple for literals |
| `string`, `regexp` | `#79740e` | Gruvbox dark green — earthy strings |
| `comment` | `#928374` | Gruvbox gray |
| `operator` | `#9d0006` | Match keywords |
| `bracket` | `#3c3836` | Warm near-black |
| `name`, `quote` | `#427b58` | Gruvbox dark aqua |
| `meta` | `#928374` | Match comments |
| `tagName` | `#427b58` | Gruvbox dark aqua |
| `heading`, `strong` | `#3c3836` | bold |
| `emphasis` | `#3c3836` | italic |
| `link`, `url` | `#076678` | Gruvbox dark blue |
| `deleted` | `#9d0006` | Gruvbox red |
| `inserted` | `#79740e` | Gruvbox green |
| `invalid` | `#cc241d` | Bright red |

### Cream Dark (warm-adapted dark syntax)

| Tag | Color |
|-----|-------|
| `keyword`, `typeName`, `typeOperator` | `#fb4934` |
| `variableName`, `attributeName` | `#ebdbb2` |
| `definition(variableName)` | `#d3869b` |
| `propertyName` | `#d3869b` |
| `number`, `bool`, `atom` | `#d3869b` |
| `string`, `regexp` | `#b8bb26` |
| `comment` | `#928374` |
| `operator` | `#fb4934` |
| `bracket` | `#ebdbb2` |
| `name`, `quote` | `#8ec07c` |
| `meta` | `#928374` |
| `tagName` | `#8ec07c` |
| `heading`, `strong` | `#ebdbb2` |
| `emphasis` | `#ebdbb2` |
| `link`, `url` | `#83a598` |
| `deleted` | `#fb4934` |
| `inserted` | `#b8bb26` |
| `invalid` | `#fb4934` |

---

## Tasks

### Task 1: Define owned highlight styles

**Files:**
- Create: `src/components/editor/highlight-styles.ts`

This file exports the four syntax highlight style arrays and settings objects. It has zero dependencies on `@uiw/codemirror-theme-github`.

- [ ] **Step 1: Create the highlight styles file**

```typescript
// src/components/editor/highlight-styles.ts
import { tags as t } from "@lezer/highlight";
import type { TagStyle } from "@codemirror/language";

// ── Editor UI settings ──

export interface ThemeSettings {
  background: string;
  foreground: string;
  caret?: string;
  selection: string;
  selectionMatch: string;
  gutterBackground: string;
  gutterForeground: string;
  lineHighlight?: string;
}

export const classicLightSettings: ThemeSettings = {
  background: "#ffffff",
  foreground: "#24292f",
  selection: "#BBDFFF",
  selectionMatch: "#BBDFFF",
  gutterBackground: "#ffffff",
  gutterForeground: "#6e7781",
};

export const classicDarkSettings: ThemeSettings = {
  background: "#0d1117",
  foreground: "#c9d1d9",
  caret: "#c9d1d9",
  selection: "#003d73",
  selectionMatch: "#003d73",
  gutterBackground: "#0d1117",
  gutterForeground: "#6e7781",
  lineHighlight: "#36334280",
};

export const creamLightSettings: ThemeSettings = {
  background: "#f5f2ec",
  foreground: "#3c3836",
  selection: "#d8cec3",
  selectionMatch: "#d8cec3",
  gutterBackground: "#f5f2ec",
  gutterForeground: "#6e655f",
};

export const creamDarkSettings: ThemeSettings = {
  background: "#1a1816",
  foreground: "#ebdbb2",
  caret: "#ebdbb2",
  selection: "#3a3533",
  selectionMatch: "#3a3533",
  gutterBackground: "#1a1816",
  gutterForeground: "#9a918a",
  lineHighlight: "#32302f80",
};

// ── Syntax highlight styles ──

export const classicLightStyle: TagStyle[] = [
  { tag: [t.keyword, t.typeName, t.typeOperator], color: "#cf222e" },
  { tag: [t.variableName, t.attributeName], color: "#24292f" },
  { tag: [t.definition(t.variableName), t.function(t.variableName)], color: "#8250df" },
  { tag: [t.propertyName], color: "#8250df" },
  { tag: [t.className], color: "#8250df" },
  { tag: [t.number, t.bool, t.atom, t.special(t.variableName)], color: "#0550ae" },
  { tag: [t.string, t.regexp], color: "#0a3069" },
  { tag: [t.comment], color: "#6e7781" },
  { tag: [t.operator], color: "#cf222e" },
  { tag: [t.bracket], color: "#24292f" },
  { tag: [t.name, t.quote], color: "#116329" },
  { tag: [t.meta], color: "#6e7781" },
  { tag: [t.standard(t.tagName), t.tagName], color: "#116329" },
  { tag: [t.heading, t.strong], color: "#24292f", fontWeight: "bold" },
  { tag: [t.emphasis], color: "#24292f", fontStyle: "italic" },
  { tag: [t.link, t.url, t.escape], color: "#0550ae" },
  { tag: t.link, textDecoration: "underline" },
  { tag: [t.deleted], color: "#82071e", backgroundColor: "#ffebe9" },
  { tag: [t.inserted], color: "#116329", backgroundColor: "#dafbe1" },
  { tag: t.strikethrough, textDecoration: "line-through" },
  { tag: t.invalid, color: "#cf222e" },
];

export const classicDarkStyle: TagStyle[] = [
  { tag: [t.keyword, t.typeName, t.typeOperator], color: "#ff7b72" },
  { tag: [t.variableName, t.attributeName], color: "#c9d1d9" },
  { tag: [t.definition(t.variableName), t.function(t.variableName)], color: "#d2a8ff" },
  { tag: [t.propertyName], color: "#d2a8ff" },
  { tag: [t.className], color: "#d2a8ff" },
  { tag: [t.number, t.bool, t.atom, t.special(t.variableName)], color: "#79c0ff" },
  { tag: [t.string, t.regexp], color: "#a5d6ff" },
  { tag: [t.comment], color: "#8b949e" },
  { tag: [t.operator], color: "#ff7b72" },
  { tag: [t.bracket], color: "#c9d1d9" },
  { tag: [t.name, t.quote], color: "#7ee787" },
  { tag: [t.meta], color: "#8b949e" },
  { tag: [t.standard(t.tagName), t.tagName], color: "#7ee787" },
  { tag: [t.heading, t.strong], color: "#d2a8ff", fontWeight: "bold" },
  { tag: [t.emphasis], color: "#d2a8ff", fontStyle: "italic" },
  { tag: [t.link, t.url, t.escape], color: "#79c0ff" },
  { tag: t.link, textDecoration: "underline" },
  { tag: [t.deleted], color: "#ffdcd7", backgroundColor: "#67060c" },
  { tag: [t.inserted], color: "#aff5b4", backgroundColor: "#033a16" },
  { tag: t.strikethrough, textDecoration: "line-through" },
  { tag: t.invalid, color: "#f97583" },
];

export const creamLightStyle: TagStyle[] = [
  { tag: [t.keyword, t.typeName, t.typeOperator], color: "#9d0006" },
  { tag: [t.variableName, t.attributeName], color: "#3c3836" },
  { tag: [t.definition(t.variableName), t.function(t.variableName)], color: "#8f3f71" },
  { tag: [t.propertyName], color: "#8f3f71" },
  { tag: [t.className], color: "#8f3f71" },
  { tag: [t.number, t.bool, t.atom, t.special(t.variableName)], color: "#8f3f71" },
  { tag: [t.string, t.regexp], color: "#79740e" },
  { tag: [t.comment], color: "#928374" },
  { tag: [t.operator], color: "#9d0006" },
  { tag: [t.bracket], color: "#3c3836" },
  { tag: [t.name, t.quote], color: "#427b58" },
  { tag: [t.meta], color: "#928374" },
  { tag: [t.standard(t.tagName), t.tagName], color: "#427b58" },
  { tag: [t.heading, t.strong], color: "#3c3836", fontWeight: "bold" },
  { tag: [t.emphasis], color: "#3c3836", fontStyle: "italic" },
  { tag: [t.link, t.url, t.escape], color: "#076678" },
  { tag: t.link, textDecoration: "underline" },
  { tag: [t.deleted], color: "#9d0006", backgroundColor: "#f9e0cc" },
  { tag: [t.inserted], color: "#79740e", backgroundColor: "#e7eaae" },
  { tag: t.strikethrough, textDecoration: "line-through" },
  { tag: t.invalid, color: "#cc241d" },
];

export const creamDarkStyle: TagStyle[] = [
  { tag: [t.keyword, t.typeName, t.typeOperator], color: "#fb4934" },
  { tag: [t.variableName, t.attributeName], color: "#ebdbb2" },
  { tag: [t.definition(t.variableName), t.function(t.variableName)], color: "#d3869b" },
  { tag: [t.propertyName], color: "#d3869b" },
  { tag: [t.className], color: "#d3869b" },
  { tag: [t.number, t.bool, t.atom, t.special(t.variableName)], color: "#d3869b" },
  { tag: [t.string, t.regexp], color: "#b8bb26" },
  { tag: [t.comment], color: "#928374" },
  { tag: [t.operator], color: "#fb4934" },
  { tag: [t.bracket], color: "#ebdbb2" },
  { tag: [t.name, t.quote], color: "#8ec07c" },
  { tag: [t.meta], color: "#928374" },
  { tag: [t.standard(t.tagName), t.tagName], color: "#8ec07c" },
  { tag: [t.heading, t.strong], color: "#ebdbb2", fontWeight: "bold" },
  { tag: [t.emphasis], color: "#ebdbb2", fontStyle: "italic" },
  { tag: [t.link, t.url, t.escape], color: "#83a598" },
  { tag: t.link, textDecoration: "underline" },
  { tag: [t.deleted], color: "#fb4934", backgroundColor: "#462726" },
  { tag: [t.inserted], color: "#b8bb26", backgroundColor: "#32361a" },
  { tag: t.strikethrough, textDecoration: "line-through" },
  { tag: t.invalid, color: "#fb4934" },
];
```

- [ ] **Step 2: Verify it compiles**

Run: `pnpm exec tsc --noEmit -p apps/notebook/tsconfig.json 2>&1 | head -20`
Expected: No errors related to highlight-styles.ts

- [ ] **Step 3: Commit**

```bash
git add src/components/editor/highlight-styles.ts
git commit -m "feat(editor): add owned syntax highlight styles for classic and cream themes"
```

---

### Task 2: Rewrite themes.ts to use owned styles

**Files:**
- Modify: `src/components/editor/themes.ts`

Replace `@uiw/codemirror-theme-github` imports with our own theme construction using `EditorView.theme()` + `HighlightStyle.define()` + `syntaxHighlighting()`.

- [ ] **Step 1: Rewrite themes.ts**

```typescript
// src/components/editor/themes.ts
import type { Extension } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";

import {
  classicLightSettings,
  classicLightStyle,
  classicDarkSettings,
  classicDarkStyle,
  creamLightSettings,
  creamLightStyle,
  creamDarkSettings,
  creamDarkStyle,
  type ThemeSettings,
} from "./highlight-styles";
import type { TagStyle } from "@codemirror/language";
import { documentHasDarkMode, isDarkMode, prefersDarkMode, useDarkMode } from "@/lib/dark-mode";

// Re-export theme detection utilities from canonical location
export { documentHasDarkMode, isDarkMode, prefersDarkMode, useDarkMode };

/**
 * Theme mode options
 */
export type ThemeMode = "light" | "dark" | "system";

/**
 * Color theme options
 */
export type ColorTheme = "classic" | "cream";

/**
 * Build a CodeMirror theme Extension from settings and syntax styles.
 * This replaces @uiw/codemirror-themes' createTheme — we own it now.
 */
function buildTheme(
  mode: "light" | "dark",
  settings: ThemeSettings,
  styles: TagStyle[],
): Extension {
  const themeExtension = EditorView.theme(
    {
      "&": {
        backgroundColor: settings.background,
        color: settings.foreground,
      },
      ".cm-content": {
        caretColor: settings.caret ?? settings.foreground,
      },
      ".cm-cursor, .cm-dropCursor": {
        borderLeftColor: settings.caret ?? settings.foreground,
      },
      "&.cm-focused .cm-selectionBackground, & .cm-line::selection, & .cm-selectionLayer .cm-selectionBackground, .cm-content ::selection":
        {
          background: settings.selection + " !important",
        },
      "& .cm-selectionMatch": {
        backgroundColor: settings.selectionMatch,
      },
      ".cm-gutters": {
        backgroundColor: settings.gutterBackground,
        color: settings.gutterForeground,
        borderRight: "none",
      },
      ".cm-activeLine": settings.lineHighlight
        ? { backgroundColor: settings.lineHighlight }
        : {},
      ".cm-activeLineGutter": settings.lineHighlight
        ? { backgroundColor: settings.lineHighlight }
        : {},
    },
    { dark: mode === "dark" },
  );

  const highlightStyle = HighlightStyle.define(styles, {
    themeType: mode,
  });

  return [themeExtension, syntaxHighlighting(highlightStyle)];
}

/**
 * Classic themes — our GitHub-inspired Light/Dark
 */
export const classicLight: Extension = buildTheme("light", classicLightSettings, classicLightStyle);
export const classicDark: Extension = buildTheme("dark", classicDarkSettings, classicDarkStyle);

/**
 * Cream themes — warm backgrounds with warm syntax colors
 */
export const creamLight: Extension = buildTheme("light", creamLightSettings, creamLightStyle);
export const creamDark: Extension = buildTheme("dark", creamDarkSettings, creamDarkStyle);

// Legacy exports for backward compatibility
export const lightTheme: Extension = classicLight;
export const darkTheme: Extension = classicDark;

/**
 * Get the appropriate theme extension based on mode and color theme
 */
export function getTheme(mode: ThemeMode, colorTheme: ColorTheme = "classic"): Extension {
  const resolvedDark =
    mode === "system"
      ? typeof window !== "undefined" && window.matchMedia("(prefers-color-scheme: dark)").matches
      : mode === "dark";

  if (colorTheme === "cream") {
    return resolvedDark ? creamDark : creamLight;
  }
  return resolvedDark ? classicDark : classicLight;
}

/**
 * Get the current theme based on automatic detection
 */
export function getAutoTheme(colorTheme: ColorTheme = "classic"): Extension {
  const dark = isDarkMode();
  if (colorTheme === "cream") {
    return dark ? creamDark : creamLight;
  }
  return dark ? classicDark : classicLight;
}
```

- [ ] **Step 2: Verify it compiles and the editor still works**

Run: `pnpm exec tsc --noEmit -p apps/notebook/tsconfig.json 2>&1 | head -20`
Expected: No errors

Then launch the app (`cargo xtask notebook --attach` with vite running) and verify code cells render with syntax highlighting.

- [ ] **Step 3: Commit**

```bash
git add src/components/editor/themes.ts
git commit -m "feat(editor): replace @uiw/codemirror-theme-github with owned theme builder"
```

---

### Task 3: Build static code highlighter component

**Files:**
- Create: `src/components/editor/static-highlight.tsx`

A React component that renders syntax-highlighted code using CodeMirror's Lezer parsers and our highlight styles — no editor instance needed. This replaces `react-syntax-highlighter`.

- [ ] **Step 1: Create the static highlighter**

```tsx
// src/components/editor/static-highlight.tsx
import { type ReactNode, useMemo } from "react";
import { HighlightStyle } from "@codemirror/language";
import { highlightCode } from "@lezer/highlight";
import type { Parser } from "@lezer/common";

// Language parsers — lazy imports to avoid loading all at once
import { pythonLanguage } from "@codemirror/lang-python";
import { javascriptLanguage, typescriptLanguage, jsxLanguage, tsxLanguage } from "@codemirror/lang-javascript";
import { jsonLanguage } from "@codemirror/lang-json";
import { yamlLanguage } from "@codemirror/lang-yaml";
import { htmlLanguage } from "@codemirror/lang-html";
import { sqlLanguage } from "@codemirror/lang-sql";
import { markdownLanguage } from "@codemirror/lang-markdown";

import {
  classicLightStyle,
  classicDarkStyle,
  creamLightStyle,
  creamDarkStyle,
} from "./highlight-styles";
import type { ColorTheme } from "./themes";

/**
 * Map of language aliases to Lezer parsers.
 * Covers the same languages as the old Prism setup where we have
 * CodeMirror language packs available.
 */
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
  sql: sqlLanguage.parser,
  markdown: markdownLanguage.parser,
  md: markdownLanguage.parser,
};

function getHighlightStyle(isDark: boolean, colorTheme: ColorTheme): HighlightStyle {
  if (colorTheme === "cream") {
    return HighlightStyle.define(isDark ? creamDarkStyle : creamLightStyle);
  }
  return HighlightStyle.define(isDark ? classicDarkStyle : classicLightStyle);
}

interface HighlightedSegment {
  text: string;
  classes: string;
}

/**
 * Highlight code using CodeMirror's Lezer parser and our highlight styles.
 * Returns an array of lines, each containing segments with text and CSS classes.
 */
function highlight(
  code: string,
  language: string,
  isDark: boolean,
  colorTheme: ColorTheme,
): ReactNode[] {
  const parser = languageParsers[language];
  const style = getHighlightStyle(isDark, colorTheme);

  if (!parser) {
    // No parser available — return plain text lines
    return code.split("\n").map((line, i) => (
      <span key={i}>
        {line}
        {"\n"}
      </span>
    ));
  }

  const tree = parser.parse(code);
  const lines: ReactNode[][] = [[]];
  let lineIndex = 0;
  let keyCounter = 0;

  highlightCode(
    code,
    tree,
    style,
    (text, classes) => {
      const parts = text.split("\n");
      for (let i = 0; i < parts.length; i++) {
        if (i > 0) {
          lineIndex++;
          lines[lineIndex] = [];
        }
        if (parts[i]) {
          lines[lineIndex].push(
            classes ? (
              <span key={keyCounter++} className={classes}>
                {parts[i]}
              </span>
            ) : (
              <span key={keyCounter++}>{parts[i]}</span>
            ),
          );
        }
      }
    },
    () => {
      lineIndex++;
      lines[lineIndex] = [];
    },
  );

  return lines.map((segments, i) => (
    <span key={i} className="cm-line">
      {segments.length > 0 ? segments : "\n"}
      {i < lines.length - 1 ? "\n" : null}
    </span>
  ));
}

interface StaticCodeBlockProps {
  /** The code to highlight */
  code: string;
  /** Language identifier (e.g. "python", "js", "typescript") */
  language?: string;
  /** Dark mode */
  isDark?: boolean;
  /** Color theme */
  colorTheme?: ColorTheme;
  /** Custom CSS class for the container */
  className?: string;
}

/**
 * Renders syntax-highlighted code using CodeMirror's Lezer parsers.
 * No editor instance needed — pure static rendering.
 *
 * Supported languages: python, javascript/js, typescript/ts, jsx, tsx,
 * json, yaml/yml, html, sql, markdown/md.
 * Unsupported languages render as plain text.
 */
export function StaticCodeBlock({
  code,
  language = "",
  isDark = false,
  colorTheme = "classic",
  className,
}: StaticCodeBlockProps) {
  const highlighted = useMemo(
    () => highlight(code, language.toLowerCase(), isDark, colorTheme),
    [code, language, isDark, colorTheme],
  );

  const bg = isDark
    ? colorTheme === "cream"
      ? "#1a1816"
      : "#161b22"
    : colorTheme === "cream"
      ? "#f0ede7"
      : "#f6f8fa";

  const fg = isDark
    ? colorTheme === "cream"
      ? "#ebdbb2"
      : "#c9d1d9"
    : colorTheme === "cream"
      ? "#3c3836"
      : "#24292f";

  return (
    <pre
      className={className}
      style={{
        margin: 0,
        padding: "0.75rem",
        fontSize: "0.875rem",
        lineHeight: 1.5,
        overflow: "auto",
        background: bg,
        color: fg,
        borderRadius: "0.375rem",
        fontFamily: "'SF Mono', Consolas, Monaco, 'Andale Mono', monospace",
      }}
    >
      <code>{highlighted}</code>
    </pre>
  );
}

/**
 * List of languages supported by the static highlighter.
 * Used for capability checks.
 */
export const supportedLanguages = Object.keys(languageParsers);
```

- [ ] **Step 2: Verify it compiles**

Run: `pnpm exec tsc --noEmit -p apps/notebook/tsconfig.json 2>&1 | head -20`
Expected: No errors

- [ ] **Step 3: Commit**

```bash
git add src/components/editor/static-highlight.tsx
git commit -m "feat(editor): add static code highlighter using CodeMirror Lezer parsers"
```

---

### Task 4: Replace Prism in markdown output

**Files:**
- Modify: `src/components/outputs/markdown-output.tsx`

Replace the `SyntaxHighlighter` import and `CodeBlock` component with `StaticCodeBlock`.

- [ ] **Step 1: Update markdown-output.tsx**

Replace the imports at the top:
```typescript
// Remove this line:
import { SyntaxHighlighter, oneDark, oneLight } from "./syntax-highlighter";

// Add this line:
import { StaticCodeBlock } from "@/components/editor/static-highlight";
import { useColorTheme } from "@/lib/dark-mode";
```

Replace the `CodeBlock` component (lines 40-89) with:
```tsx
interface CodeBlockProps {
  children: string;
  language?: string;
  enableCopy?: boolean;
  isDark?: boolean;
}

function CodeBlock({ children, language = "", enableCopy = true, isDark = false }: CodeBlockProps) {
  const [copied, setCopied] = useState(false);
  const colorTheme = (useColorTheme() ?? "classic") as "classic" | "cream";

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(children);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch (err) {
      console.error("Failed to copy code:", err);
    }
  };

  return (
    <div className="group/codeblock relative">
      <StaticCodeBlock
        code={children}
        language={language}
        isDark={isDark}
        colorTheme={colorTheme}
      />
      {enableCopy && (
        <button
          onClick={handleCopy}
          className="absolute top-2 right-2 z-10 rounded border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-800 p-1.5 text-gray-600 dark:text-gray-400 opacity-0 shadow-sm transition-opacity group-hover/codeblock:opacity-100 hover:bg-gray-50 dark:hover:bg-gray-700 hover:text-gray-800 dark:hover:text-gray-200"
          title={copied ? "Copied!" : "Copy code"}
          type="button"
        >
          {copied ? <Check className="h-3 w-3" /> : <Copy className="h-3 w-3" />}
        </button>
      )}
    </div>
  );
}
```

- [ ] **Step 2: Verify it compiles**

Run: `pnpm exec tsc --noEmit -p apps/notebook/tsconfig.json 2>&1 | head -20`
Expected: No errors

- [ ] **Step 3: Test visually**

Open a notebook with markdown output containing fenced code blocks. Verify:
- Python code blocks have syntax highlighting
- JavaScript/TypeScript code blocks work
- Unknown languages render as plain text
- Copy button still works
- Dark mode shows dark colors
- Cream theme shows warm colors

- [ ] **Step 4: Commit**

```bash
git add src/components/outputs/markdown-output.tsx
git commit -m "refactor(outputs): replace Prism with CodeMirror static highlighter in markdown"
```

---

### Task 5: Replace Prism in history search dialog

**Files:**
- Modify: `apps/notebook/src/components/HistorySearchDialog.tsx`

- [ ] **Step 1: Update imports and rendering**

Replace:
```typescript
import { oneDark, oneLight, SyntaxHighlighter } from "@/components/outputs/syntax-highlighter";
```

With:
```typescript
import { StaticCodeBlock } from "@/components/editor/static-highlight";
import { useColorTheme } from "@/lib/dark-mode";
```

In the `HistoryEntry` component, replace the `SyntaxHighlighter` usage:
```tsx
const colorTheme = (useColorTheme() ?? "classic") as "classic" | "cream";

return (
  <StaticCodeBlock
    code={displayCode}
    language="python"
    isDark={isDark}
    colorTheme={colorTheme}
    className={/* preserve existing className/style */}
  />
);
```

Check the existing `customStyle` prop for background, border-radius, padding — carry those over to the `StaticCodeBlock` via className or wrapping div.

- [ ] **Step 2: Verify it compiles and test**

Run: `pnpm exec tsc --noEmit -p apps/notebook/tsconfig.json 2>&1 | head -20`
Expected: No errors

Test: Open a code cell, press Ctrl+R, verify history entries have syntax highlighting.

- [ ] **Step 3: Commit**

```bash
git add apps/notebook/src/components/HistorySearchDialog.tsx
git commit -m "refactor(history): replace Prism with CodeMirror static highlighter"
```

---

### Task 6: Remove Prism dependencies

**Files:**
- Delete: `src/components/outputs/syntax-highlighter.ts`
- Modify: `package.json`

- [ ] **Step 1: Verify no remaining references to old Prism file**

Run: `grep -r "syntax-highlighter" src/ apps/ --include='*.ts' --include='*.tsx' -l`
Expected: No files (all references replaced in Tasks 4-5)

- [ ] **Step 2: Delete the Prism wrapper**

```bash
rm src/components/outputs/syntax-highlighter.ts
```

- [ ] **Step 3: Remove Prism packages**

```bash
pnpm remove react-syntax-highlighter @types/react-syntax-highlighter
```

- [ ] **Step 4: Remove @uiw/codemirror-theme-github**

```bash
pnpm remove @uiw/codemirror-theme-github @uiw/codemirror-themes
```

Note: only remove `@uiw/codemirror-themes` if it's a direct dependency and nothing else imports `createTheme` from it (we replaced that usage in Task 2).

- [ ] **Step 5: Verify build**

Run: `pnpm install && pnpm exec tsc --noEmit -p apps/notebook/tsconfig.json 2>&1 | head -30`
Expected: Clean compile, no missing modules

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "chore: remove react-syntax-highlighter and @uiw/codemirror-theme-github dependencies"
```

---

### Task 7: Run lint and full verification

**Files:** None (verification only)

- [ ] **Step 1: Run linting**

```bash
cargo xtask lint --fix
```

Expected: Clean pass (no new warnings)

- [ ] **Step 2: Run TypeScript type check**

```bash
pnpm exec tsc --noEmit -p apps/notebook/tsconfig.json
```

Expected: No errors

- [ ] **Step 3: Run unit tests**

```bash
pnpm test:run
```

Expected: All existing tests pass

- [ ] **Step 4: Visual verification**

Launch the app and verify:
1. Code cells: syntax highlighting works in classic light, classic dark, cream light, cream dark
2. Markdown output: fenced code blocks have highlighting matching the editor
3. History search (Ctrl+R): entries are highlighted
4. Theme switching: changing between classic/cream updates all code colors
5. No regressions in editor behavior (cursor, selection, line numbers)

- [ ] **Step 5: Final commit if any fixups needed**

```bash
cargo xtask lint --fix
git add -A
git commit -m "fix: lint and verification fixes"
```

---

## Post-Implementation Notes

### Languages not covered

The old Prism setup registered 24 languages. Our static highlighter covers 9 (python, js, ts, jsx, tsx, json, yaml, html, sql, markdown). Languages without a `@codemirror/lang-*` package (bash, c, cpp, css, diff, go, java, kotlin, latex, r, ruby, rust, scala, swift, toml) will render as plain text. This is acceptable — the important ones (Python, JS/TS, JSON) are covered. Additional language packs can be added later as `@codemirror/lang-*` packages or community Lezer grammars.

### Future opportunities

- **CSS highlighting**: Add `@codemirror/lang-css` for CSS code blocks
- **Rust highlighting**: Add `@codemirror/lang-rust` for Rust code blocks
- **Additional languages**: Community Lezer grammars exist for many languages
- **Code block line numbers**: Easy to add since we control the rendering
- **Inline code highlighting**: Could use the same static highlighter for inline `code` in markdown
