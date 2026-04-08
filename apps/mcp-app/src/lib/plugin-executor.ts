/**
 * Execute a CJS renderer plugin and extract its registered components.
 *
 * Plugins are CJS modules built with React externalized. They export
 * an `install(ctx)` function that calls `ctx.register(mimeTypes, component)`
 * to register React components for specific MIME types.
 *
 * This executor provides a custom `require` shim that supplies React
 * from the host bundle — same pattern as the notebook app's isolated
 * iframe renderer.
 */

import React from "react";
import * as jsxRuntime from "react/jsx-runtime";

type RendererComponent = React.ComponentType<{
  data: unknown;
  metadata?: Record<string, unknown>;
  mimeType: string;
}>;

interface PluginRegistry {
  /** Exact MIME type → component */
  exact: Map<string, RendererComponent>;
  /** Pattern matchers (test function + component) */
  patterns: Array<{
    test: (mime: string) => boolean;
    component: RendererComponent;
  }>;
}

/** Global registry of installed plugin components. */
const registry: PluginRegistry = {
  exact: new Map(),
  patterns: [],
};

/**
 * Install a CJS renderer plugin by executing its code.
 *
 * The plugin's `install()` function receives a registration context
 * that lets it register React components for MIME types.
 */
export function installPlugin(code: string, css?: string): void {
  // Inject CSS if provided
  if (css) {
    const style = document.createElement("style");
    style.textContent = css;
    document.head.appendChild(style);
  }

  // Create CJS module object
  const mod: { exports: Record<string, unknown> } = { exports: {} };

  // Custom require shim — provides React from host bundle
  const customRequire = (name: string): unknown => {
    if (name === "react") return React;
    if (name === "react/jsx-runtime") return jsxRuntime;
    throw new Error(`Unknown module: ${name}`);
  };

  // Execute plugin code in CJS context
  const fn = new Function("module", "exports", "require", code);
  fn(mod, mod.exports, customRequire);

  // Call install() to register components
  const install = mod.exports.install as
    | ((ctx: {
        register: (mimeTypes: string[], component: RendererComponent) => void;
        registerPattern: (
          test: (mime: string) => boolean,
          component: RendererComponent,
        ) => void;
      }) => void)
    | undefined;

  if (install) {
    install({
      register(mimeTypes, component) {
        for (const mime of mimeTypes) {
          registry.exact.set(mime, component);
        }
      },
      registerPattern(test, component) {
        registry.patterns.push({ test, component });
      },
    });
  }
}

/**
 * Look up a registered renderer component for a MIME type.
 * Returns undefined if no plugin has registered for this MIME type.
 */
export function getPluginRenderer(
  mime: string,
): RendererComponent | undefined {
  // Check exact matches first
  const exact = registry.exact.get(mime);
  if (exact) return exact;

  // Check pattern matchers
  for (const { test, component } of registry.patterns) {
    if (test(mime)) return component;
  }

  return undefined;
}
