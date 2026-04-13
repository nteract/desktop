import type { TagStyle } from "@codemirror/language";
import { tags as t } from "@lezer/highlight";

/**
 * Editor UI settings for CodeMirror themes.
 */
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

// ---------------------------------------------------------------------------
// Classic Light (GitHub-inspired)
// ---------------------------------------------------------------------------

export const classicLightSettings: ThemeSettings = {
  background: "#ffffff",
  foreground: "#24292f",
  selection: "#BBDFFF",
  selectionMatch: "#BBDFFF",
  gutterBackground: "#ffffff",
  gutterForeground: "#6e7781",
};

export const classicLightStyle: TagStyle[] = [
  { tag: [t.keyword, t.typeName, t.typeOperator], color: "#cf222e" },
  { tag: [t.variableName, t.attributeName], color: "#24292f" },
  {
    tag: [t.definition(t.variableName), t.function(t.variableName)],
    color: "#8250df",
  },
  { tag: [t.propertyName], color: "#8250df" },
  { tag: [t.className], color: "#8250df" },
  {
    tag: [t.number, t.bool, t.atom, t.special(t.variableName)],
    color: "#0550ae",
  },
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
  {
    tag: [t.deleted],
    color: "#82071e",
    backgroundColor: "#ffebe9",
  },
  {
    tag: [t.inserted],
    color: "#116329",
    backgroundColor: "#dafbe1",
  },
  { tag: t.strikethrough, textDecoration: "line-through" },
  { tag: t.invalid, color: "#cf222e" },
];

// ---------------------------------------------------------------------------
// Classic Dark (GitHub Dark-inspired)
// ---------------------------------------------------------------------------

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

export const classicDarkStyle: TagStyle[] = [
  { tag: [t.keyword, t.typeName, t.typeOperator], color: "#ff7b72" },
  { tag: [t.variableName, t.attributeName], color: "#c9d1d9" },
  {
    tag: [t.definition(t.variableName), t.function(t.variableName)],
    color: "#d2a8ff",
  },
  { tag: [t.propertyName], color: "#d2a8ff" },
  { tag: [t.className], color: "#d2a8ff" },
  {
    tag: [t.number, t.bool, t.atom, t.special(t.variableName)],
    color: "#79c0ff",
  },
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
  {
    tag: [t.deleted],
    color: "#ffdcd7",
    backgroundColor: "#67060c",
  },
  {
    tag: [t.inserted],
    color: "#aff5b4",
    backgroundColor: "#033a16",
  },
  { tag: t.strikethrough, textDecoration: "line-through" },
  { tag: t.invalid, color: "#f97583" },
];

// ---------------------------------------------------------------------------
// Cream Light (warm Gruvbox-inspired)
// ---------------------------------------------------------------------------

export const creamLightSettings: ThemeSettings = {
  background: "#f5f2ec",
  foreground: "#3c3836",
  selection: "#d8cec3",
  selectionMatch: "#d8cec3",
  gutterBackground: "#f5f2ec",
  gutterForeground: "#6e655f",
};

export const creamLightStyle: TagStyle[] = [
  { tag: [t.keyword, t.typeName, t.typeOperator], color: "#9d0006" },
  { tag: [t.variableName, t.attributeName], color: "#3c3836" },
  {
    tag: [t.definition(t.variableName), t.function(t.variableName)],
    color: "#8f3f71",
  },
  { tag: [t.propertyName], color: "#8f3f71" },
  { tag: [t.className], color: "#8f3f71" },
  {
    tag: [t.number, t.bool, t.atom, t.special(t.variableName)],
    color: "#8f3f71",
  },
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
  {
    tag: [t.deleted],
    color: "#9d0006",
    backgroundColor: "#f9e0cc",
  },
  {
    tag: [t.inserted],
    color: "#79740e",
    backgroundColor: "#e7eaae",
  },
  { tag: t.strikethrough, textDecoration: "line-through" },
  { tag: t.invalid, color: "#cc241d" },
];

// ---------------------------------------------------------------------------
// Cream Dark (warm dark Gruvbox-inspired)
// ---------------------------------------------------------------------------

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

export const creamDarkStyle: TagStyle[] = [
  { tag: [t.keyword, t.typeName, t.typeOperator], color: "#fb4934" },
  { tag: [t.variableName, t.attributeName], color: "#ebdbb2" },
  {
    tag: [t.definition(t.variableName), t.function(t.variableName)],
    color: "#d3869b",
  },
  { tag: [t.propertyName], color: "#d3869b" },
  { tag: [t.className], color: "#d3869b" },
  {
    tag: [t.number, t.bool, t.atom, t.special(t.variableName)],
    color: "#d3869b",
  },
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
  {
    tag: [t.deleted],
    color: "#fb4934",
    backgroundColor: "#462726",
  },
  {
    tag: [t.inserted],
    color: "#b8bb26",
    backgroundColor: "#32361a",
  },
  { tag: t.strikethrough, textDecoration: "line-through" },
  { tag: t.invalid, color: "#fb4934" },
];
