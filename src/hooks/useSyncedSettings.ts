import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Theme } from "@tauri-apps/api/window";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useCallback, useEffect, useState } from "react";
import type { ColorTheme, PythonEnvType, Runtime, SyncedSettings, ThemeMode } from "@/bindings";

// Re-export generated types so consumers can import from this module.
export type { ColorTheme, ThemeMode, Runtime, PythonEnvType };

function isValidColorTheme(value: string): value is ColorTheme {
  return value === "classic" || value === "cream";
}

function getStoredColorTheme(): ColorTheme {
  try {
    const stored = localStorage.getItem("notebook-color-theme");
    if (stored && isValidColorTheme(stored)) return stored;
  } catch {
    // ignore
  }
  return "classic";
}

function setStoredColorTheme(value: ColorTheme) {
  try {
    localStorage.setItem("notebook-color-theme", value);
  } catch {
    // ignore
  }
}

function resolveTheme(theme: ThemeMode): "light" | "dark" {
  if (theme === "system") {
    return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  }
  return theme;
}

function applyThemeToDOM(resolved: "light" | "dark") {
  const html = document.documentElement;
  if (resolved === "dark") {
    html.classList.add("dark");
    html.classList.remove("light");
  } else {
    html.classList.remove("dark");
    html.classList.add("light");
  }
}

async function syncNativeWindowTheme(theme: ThemeMode): Promise<void> {
  try {
    const tauriTheme: Theme | null = theme === "system" ? null : theme;
    await getCurrentWindow().setTheme(tauriTheme);
  } catch {
    // Silently fail if not in Tauri context
  }
}

function isValidTheme(value: string): value is ThemeMode {
  return value === "light" || value === "dark" || value === "system";
}

/** Known runtime values for UI buttons; unknown values are preserved. */
export function isKnownRuntime(value: string): value is "python" | "deno" {
  return value === "python" || value === "deno";
}

/** Known env type values for UI buttons; unknown values are preserved. */
export function isKnownPythonEnv(value: string): value is "uv" | "conda" | "pixi" {
  return value === "uv" || value === "conda" || value === "pixi";
}

/**
 * Read a theme value from localStorage.
 *
 * localStorage is ONLY used for the theme setting to avoid a flash of
 * unstyled content (FOUC) on startup. All other settings initialize from
 * defaults and wait for the daemon to provide the authoritative value.
 */
function getStoredTheme(): ThemeMode {
  try {
    const stored = localStorage.getItem("notebook-theme");
    if (stored && isValidTheme(stored)) return stored;
  } catch {
    // ignore
  }
  return "system";
}

function setStoredTheme(value: ThemeMode) {
  try {
    localStorage.setItem("notebook-theme", value);
  } catch {
    // ignore
  }
}

/**
 * Hook for all synced settings across notebook windows via runtimed.
 *
 * The daemon (Automerge doc) is the source of truth. On mount, we fetch
 * the current settings from the daemon and listen for cross-window changes.
 *
 * localStorage is only used for theme to avoid FOUC. All other settings
 * initialize from defaults and are overwritten once the daemon responds.
 */
export function useSyncedSettings() {
  // Theme uses localStorage to avoid flash of wrong theme on startup
  const [theme, setThemeState] = useState<ThemeMode>(getStoredTheme);
  const [colorTheme, setColorThemeState] = useState<ColorTheme>(getStoredColorTheme);
  // All other settings use defaults — daemon is the source of truth.
  // State is `string` (not just the known union) to preserve unknown values
  // from other branches without silently dropping them.
  const [defaultRuntime, setDefaultRuntimeState] = useState<string>("python");
  const [defaultPythonEnv, setDefaultPythonEnvState] = useState<string>("uv");
  const [defaultUvPackages, setDefaultUvPackagesState] = useState<string[]>([]);
  const [defaultCondaPackages, setDefaultCondaPackagesState] = useState<string[]>([]);
  const [defaultPixiPackages, setDefaultPixiPackagesState] = useState<string[]>([]);
  // Keep-alive duration in seconds (5s to 7 days)
  const [keepAliveSecs, setKeepAliveSecsState] = useState<number>(30);
  // Feature flag: nteract/dx kernel bootstrap
  const [bootstrapDx, setBootstrapDxState] = useState<boolean>(false);

  // Load initial settings from daemon
  useEffect(() => {
    invoke<SyncedSettings>("get_synced_settings")
      .then((settings) => {
        if (isValidTheme(settings.theme)) {
          setThemeState(settings.theme);
          setStoredTheme(settings.theme);
        }
        if (isValidColorTheme(settings.color_theme)) {
          setColorThemeState(settings.color_theme);
          setStoredColorTheme(settings.color_theme);
        }
        if (typeof settings.default_runtime === "string") {
          setDefaultRuntimeState(settings.default_runtime);
        }
        if (typeof settings.default_python_env === "string") {
          setDefaultPythonEnvState(settings.default_python_env);
        }
        if (Array.isArray(settings.uv?.default_packages)) {
          setDefaultUvPackagesState(settings.uv.default_packages);
        }
        if (Array.isArray(settings.conda?.default_packages)) {
          setDefaultCondaPackagesState(settings.conda.default_packages);
        }
        if (Array.isArray(settings.pixi?.default_packages)) {
          setDefaultPixiPackagesState(settings.pixi.default_packages);
        }
        // Handle keep_alive_secs: bigint from backend, convert to number
        if (typeof settings.keep_alive_secs === "bigint") {
          setKeepAliveSecsState(Number(settings.keep_alive_secs));
        } else if (typeof settings.keep_alive_secs === "number") {
          setKeepAliveSecsState(settings.keep_alive_secs);
        }
        if (typeof settings.bootstrap_dx === "boolean") {
          setBootstrapDxState(settings.bootstrap_dx);
        }
      })
      .catch(() => {
        // Daemon unavailable — defaults are fine
      });
  }, []);

  // Listen for cross-window settings changes via Tauri events
  useEffect(() => {
    const unlisten = listen<SyncedSettings>("settings:changed", (event) => {
      const {
        theme: newTheme,
        color_theme: newColorTheme,
        default_runtime,
        default_python_env,
        keep_alive_secs,
      } = event.payload;
      if (isValidTheme(newTheme)) {
        setThemeState(newTheme);
        setStoredTheme(newTheme);
      }
      if (isValidColorTheme(newColorTheme)) {
        setColorThemeState(newColorTheme);
        setStoredColorTheme(newColorTheme);
      }
      if (typeof default_runtime === "string") {
        setDefaultRuntimeState(default_runtime);
      }
      if (typeof default_python_env === "string") {
        setDefaultPythonEnvState(default_python_env);
      }
      if (Array.isArray(event.payload.uv?.default_packages)) {
        setDefaultUvPackagesState(event.payload.uv.default_packages);
      }
      if (Array.isArray(event.payload.conda?.default_packages)) {
        setDefaultCondaPackagesState(event.payload.conda.default_packages);
      }
      if (Array.isArray(event.payload.pixi?.default_packages)) {
        setDefaultPixiPackagesState(event.payload.pixi.default_packages);
      }
      // Handle keep_alive_secs: bigint from backend, convert to number
      if (typeof keep_alive_secs === "bigint") {
        setKeepAliveSecsState(Number(keep_alive_secs));
      } else if (typeof keep_alive_secs === "number") {
        setKeepAliveSecsState(keep_alive_secs);
      }
      if (typeof event.payload.bootstrap_dx === "boolean") {
        setBootstrapDxState(event.payload.bootstrap_dx);
      }
    });
    return () => {
      unlisten.then((u) => u());
    };
  }, []);

  const setTheme = useCallback((newTheme: ThemeMode) => {
    setThemeState(newTheme);
    setStoredTheme(newTheme);
    invoke("set_synced_setting", { key: "theme", value: newTheme }).catch((e) =>
      console.warn("[settings] Failed to persist theme:", e),
    );
  }, []);

  const setColorTheme = useCallback((newColorTheme: ColorTheme) => {
    setColorThemeState(newColorTheme);
    setStoredColorTheme(newColorTheme);
    invoke("set_synced_setting", { key: "color_theme", value: newColorTheme }).catch((e) =>
      console.warn("[settings] Failed to persist color theme:", e),
    );
  }, []);

  const setDefaultRuntime = useCallback((newRuntime: string) => {
    setDefaultRuntimeState(newRuntime);
    invoke("set_synced_setting", {
      key: "default_runtime",
      value: newRuntime,
    }).catch((e) => console.warn("[settings] Failed to persist runtime:", e));
  }, []);

  const setDefaultPythonEnv = useCallback((newEnv: string) => {
    setDefaultPythonEnvState(newEnv);
    invoke("set_synced_setting", {
      key: "default_python_env",
      value: newEnv,
    }).catch((e) => console.warn("[settings] Failed to persist python env:", e));
  }, []);

  const setDefaultUvPackages = useCallback((packages: string[]) => {
    setDefaultUvPackagesState(packages);
    invoke("set_synced_setting", {
      key: "uv.default_packages",
      value: packages,
    }).catch((e) => console.warn("[settings] Failed to persist uv packages:", e));
  }, []);

  const setDefaultCondaPackages = useCallback((packages: string[]) => {
    setDefaultCondaPackagesState(packages);
    invoke("set_synced_setting", {
      key: "conda.default_packages",
      value: packages,
    }).catch((e) => console.warn("[settings] Failed to persist conda packages:", e));
  }, []);

  const setDefaultPixiPackages = useCallback((packages: string[]) => {
    setDefaultPixiPackagesState(packages);
    invoke("set_synced_setting", {
      key: "pixi.default_packages",
      value: packages,
    }).catch((e) => console.warn("[settings] Failed to persist pixi packages:", e));
  }, []);

  const setKeepAliveSecs = useCallback((secs: number) => {
    setKeepAliveSecsState(secs);
    invoke("set_synced_setting", {
      key: "keep_alive_secs",
      value: secs,
    }).catch((e) => console.warn("[settings] Failed to persist keep_alive_secs:", e));
  }, []);

  const setBootstrapDx = useCallback((enabled: boolean) => {
    setBootstrapDxState(enabled);
    invoke("set_synced_setting", {
      key: "bootstrap_dx",
      value: enabled,
    }).catch((e) => console.warn("[settings] Failed to persist bootstrap_dx:", e));
  }, []);

  return {
    theme,
    setTheme,
    colorTheme,
    setColorTheme,
    defaultRuntime,
    setDefaultRuntime,
    defaultPythonEnv,
    setDefaultPythonEnv,
    defaultUvPackages,
    setDefaultUvPackages,
    defaultCondaPackages,
    setDefaultCondaPackages,
    defaultPixiPackages,
    setDefaultPixiPackages,
    keepAliveSecs,
    setKeepAliveSecs,
    bootstrapDx,
    setBootstrapDx,
  };
}

/**
 * Hook for theme that syncs across all notebook windows via runtimed.
 *
 * Wraps useSyncedSettings() and adds DOM/native window theme application.
 * Falls back to localStorage if the daemon is unavailable.
 */
export function useSyncedTheme() {
  const { theme, setTheme, colorTheme, setColorTheme } = useSyncedSettings();

  const [resolvedTheme, setResolvedTheme] = useState<"light" | "dark">(() => resolveTheme(theme));

  // Apply theme to DOM and native window
  useEffect(() => {
    const resolved = resolveTheme(theme);
    setResolvedTheme(resolved);
    applyThemeToDOM(resolved);
    syncNativeWindowTheme(theme);

    if (theme !== "system") return;

    const mediaQuery = window.matchMedia("(prefers-color-scheme: dark)");
    const handleChange = () => {
      const newResolved = resolveTheme("system");
      setResolvedTheme(newResolved);
      applyThemeToDOM(newResolved);
    };
    mediaQuery.addEventListener("change", handleChange);
    return () => mediaQuery.removeEventListener("change", handleChange);
  }, [theme]);

  // Apply color theme to DOM so iframes can read it
  useEffect(() => {
    const html = document.documentElement;
    if (colorTheme === "classic") {
      html.removeAttribute("data-color-theme");
    } else {
      html.setAttribute("data-color-theme", colorTheme);
    }
  }, [colorTheme]);

  return { theme, setTheme, colorTheme, setColorTheme, resolvedTheme };
}
