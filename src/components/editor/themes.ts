import type { Extension } from "@codemirror/state";
import {
  githubDark,
  githubDarkInit,
  githubLight,
  githubLightInit,
} from "@uiw/codemirror-theme-github";

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
 * Classic themes — GitHub Light/Dark
 */
export const classicLight: Extension = githubLight;
export const classicDark: Extension = githubDark;

/**
 * Cream themes — warm backgrounds with GitHub syntax highlighting.
 * Uses githubLightInit/githubDarkInit with settings overrides.
 */
export const creamLight: Extension = githubLightInit({
  settings: {
    background: "#f5f2ec",
    gutterBackground: "#f5f2ec",
    gutterForeground: "#6e655f",
    selection: "#d8cec3",
    selectionMatch: "#d8cec3",
  },
});

export const creamDark: Extension = githubDarkInit({
  settings: {
    background: "#1a1816",
    gutterBackground: "#1a1816",
    gutterForeground: "#9a918a",
    selection: "#3a3533",
    selectionMatch: "#3a3533",
  },
});

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
