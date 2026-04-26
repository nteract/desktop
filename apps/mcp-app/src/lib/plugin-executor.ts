/**
 * Plugin registration and loading for daemon-served renderer plugins.
 *
 * Plugins are CJS modules built with React externalized. At build time,
 * they're wrapped in an IIFE that calls `window.__nteract.register()`
 * to self-register during script execution. This avoids `unsafe-eval`
 * (no `new Function()`) and global scope pollution (no `window.module`).
 *
 * The `window.__nteract` global is set once at module load time and
 * provides:
 * - `require(name)` — returns React or jsx-runtime
 * - `register(mimeTypes, component)` — exact MIME type registration
 * - `registerPattern(test, component)` — pattern-based registration
 */

import React from "react";
import * as jsxRuntime from "react/jsx-runtime";

type RendererComponent = React.ComponentType<{
  data: unknown;
  metadata?: Record<string, unknown>;
  mimeType: string;
}>;

interface PluginRegistry {
  exact: Map<string, RendererComponent>;
  patterns: Array<{
    test: (mime: string) => boolean;
    component: RendererComponent;
  }>;
}

const registry: PluginRegistry = {
  exact: new Map(),
  patterns: [],
};

// Set up the global plugin API. Plugins call these during their IIFE
// execution (synchronous, before <script> onload fires).
declare global {
  interface Window {
    __nteract: {
      require: (name: string) => unknown;
      register: (
        mimeTypes: string[],
        component: RendererComponent,
      ) => void;
      registerPattern: (
        test: (mime: string) => boolean,
        component: RendererComponent,
      ) => void;
    };
  }
}

window.__nteract = {
  require(name: string): unknown {
    if (name === "react") return React;
    if (name === "react/jsx-runtime") return jsxRuntime;
    throw new Error(`Plugin require: unknown module "${name}"`);
  },
  register(mimeTypes: string[], component: RendererComponent) {
    for (const mime of mimeTypes) {
      registry.exact.set(mime, component);
    }
  },
  registerPattern(
    test: (mime: string) => boolean,
    component: RendererComponent,
  ) {
    registry.patterns.push({ test, component });
  },
};

/**
 * Load a renderer plugin via <script> tag.
 *
 * The plugin self-registers via `window.__nteract` during execution.
 * By the time `onload` fires, the plugin's components are in the registry.
 */
export function installPluginFromUrl(
  jsUrl: string,
  cssUrl?: string,
): Promise<void> {
  const cssPromise = cssUrl ? loadStylesheet(cssUrl) : Promise.resolve();
  const scriptPromise = loadScript(jsUrl);

  return Promise.all([cssPromise, scriptPromise]).then(() => undefined);
}

function loadStylesheet(cssUrl: string): Promise<void> {
  return new Promise((resolve, reject) => {
    if (cssUrl) {
      const link = document.createElement("link");
      link.rel = "stylesheet";
      link.href = cssUrl;
      link.onload = () => resolve();
      link.onerror = () => reject(new Error(`Failed to load plugin stylesheet: ${cssUrl}`));
      document.head.appendChild(link);
    }
  });
}

function loadScript(jsUrl: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const script = document.createElement("script");
    script.src = jsUrl;
    script.onload = () => resolve();
    script.onerror = () => reject(new Error(`Failed to load plugin: ${jsUrl}`));
    document.head.appendChild(script);
  });
}

/**
 * Look up a registered renderer component for a MIME type.
 */
export function getPluginRenderer(
  mime: string,
): RendererComponent | undefined {
  const exact = registry.exact.get(mime);
  if (exact) return exact;

  for (const { test, component } of registry.patterns) {
    if (test(mime)) return component;
  }

  return undefined;
}
