import type { Extension } from "@codemirror/state";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import type { TagStyle } from "@codemirror/language";
import { EditorView } from "@codemirror/view";

import {
  type ThemeSettings,
  classicDarkSettings,
  classicDarkStyle,
  classicLightSettings,
  classicLightStyle,
  creamDarkSettings,
  creamDarkStyle,
  creamLightSettings,
  creamLightStyle,
} from "./highlight-styles";

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
 * Build a CodeMirror theme extension from structural settings and syntax styles.
 */
function buildTheme(
  mode: "light" | "dark",
  settings: ThemeSettings,
  styles: TagStyle[],
): Extension {
  const themeExtension = EditorView.theme(
    {
      "&": {
        color: settings.foreground,
        backgroundColor: settings.background,
      },
      ".cm-content": {
        caretColor: settings.caret ?? settings.foreground,
      },
      ".cm-cursor, .cm-dropCursor": {
        borderLeftColor: settings.caret ?? settings.foreground,
      },
      "&.cm-focused .cm-selectionBackground, & .cm-line::selection, & .cm-selectionLayer .cm-selectionBackground, .cm-content ::selection":
        {
          background: `${settings.selection} !important`,
        },
      "& .cm-selectionMatch": {
        backgroundColor: settings.selectionMatch,
      },
      ".cm-gutters": {
        backgroundColor: settings.gutterBackground,
        color: settings.gutterForeground,
        borderRight: "none",
      },
      ...(settings.lineHighlight
        ? {
            ".cm-activeLine": {
              backgroundColor: settings.lineHighlight,
            },
            ".cm-activeLineGutter": {
              backgroundColor: settings.lineHighlight,
            },
          }
        : {}),
    },
    { dark: mode === "dark" },
  );

  const highlightStyle = HighlightStyle.define(styles);

  return [themeExtension, syntaxHighlighting(highlightStyle)];
}

/**
 * Classic themes — GitHub-inspired Light/Dark
 */
export const classicLight: Extension = buildTheme("light", classicLightSettings, classicLightStyle);
export const classicDark: Extension = buildTheme("dark", classicDarkSettings, classicDarkStyle);

/**
 * Cream themes — warm Gruvbox-inspired Light/Dark
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
 * Checks document class, color-scheme, data-theme attribute, and system preference
 */
export function getAutoTheme(colorTheme: ColorTheme = "classic"): Extension {
  const dark = isDarkMode();
  if (colorTheme === "cream") {
    return dark ? creamDark : creamLight;
  }
  return dark ? classicDark : classicLight;
}
