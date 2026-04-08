/**
 * Execute a CJS renderer plugin via <script> tag loading.
 *
 * Plugins are CJS modules built with React externalized. They export
 * an `install(ctx)` function that calls `ctx.register(mimeTypes, component)`
 * to register React components for specific MIME types.
 *
 * We load plugins via <script src="..."> to avoid needing `unsafe-eval`
 * in the host's CSP. The CJS require/module/exports context is provided
 * via window globals that the script picks up during execution.
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

// Set up the CJS shim globals. The <script> tag executes in the global
// scope and will find these via the implicit `module`, `exports`, `require`
// references that CJS bundles use.
const win = window as Record<string, unknown>;

function setupCjsGlobals(): { exports: Record<string, unknown> } {
  const mod = { exports: {} as Record<string, unknown> };
  win.module = mod;
  win.exports = mod.exports;
  win.require = (name: string): unknown => {
    if (name === "react") return React;
    if (name === "react/jsx-runtime") return jsxRuntime;
    throw new Error(`Unknown module: ${name}`);
  };
  return mod;
}

function cleanupCjsGlobals(): void {
  delete win.module;
  delete win.exports;
  delete win.require;
}

/**
 * Load and install a CJS renderer plugin via <script> tag.
 *
 * The plugin URL must be from an origin in the host's CSP resourceDomains
 * (the daemon's blob server URL is already declared there).
 */
export function installPluginFromUrl(jsUrl: string, cssUrl?: string): Promise<void> {
  return new Promise((resolve, reject) => {
    // Inject CSS if provided
    if (cssUrl) {
      const link = document.createElement("link");
      link.rel = "stylesheet";
      link.href = cssUrl;
      document.head.appendChild(link);
    }

    // Set up CJS globals before the script runs
    const mod = setupCjsGlobals();

    const script = document.createElement("script");
    script.src = jsUrl;
    script.onload = () => {
      cleanupCjsGlobals();

      // Extract install function from the CJS exports
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

      resolve();
    };
    script.onerror = () => {
      cleanupCjsGlobals();
      reject(new Error(`Failed to load plugin: ${jsUrl}`));
    };
    document.head.appendChild(script);
  });
}

/**
 * Look up a registered renderer component for a MIME type.
 * Returns undefined if no plugin has registered for this MIME type.
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
