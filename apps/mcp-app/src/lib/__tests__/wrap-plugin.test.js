import fs from "node:fs";
import path from "node:path";
import vm from "node:vm";
import { beforeEach, describe, expect, it } from "vite-plus/test";
import { wrapForMcpApp } from "../wrap-plugin.js";

/** Minimal mock of window.__nteract for testing */
function createMockNteract() {
  const registered = new Map();
  const patterns = [];
  return {
    api: {
      require(name) {
        if (name === "react") return { createElement: () => null };
        if (name === "react/jsx-runtime")
          return { jsx: () => null, jsxs: () => null };
        throw new Error(`Unknown module: ${name}`);
      },
      register(mimeTypes, component) {
        for (const mime of mimeTypes) registered.set(mime, component);
      },
      registerPattern(test, component) {
        patterns.push({ test, component });
      },
    },
    registered,
    patterns,
  };
}

/** Execute wrapped plugin code using Node's vm module.
 * @returns The execution context for inspecting global state
 */
function execWrapped(code, nteractApi) {
  const context = vm.createContext({ window: { __nteract: nteractApi } });
  vm.runInContext(code, context);
  return context;
}

describe("wrapForMcpApp", () => {
  it("wraps code in an IIFE", () => {
    const wrapped = wrapForMcpApp("exports.x = 1;");
    expect(wrapped).toMatch(/^\(function\(\)\{/);
    expect(wrapped).toMatch(/\}\)\(\);$/);
  });

  it("does not leak module/exports/require to global scope", () => {
    const mock = createMockNteract();
    const ctx = execWrapped(wrapForMcpApp("exports.x = 42;"), mock.api);

    expect(ctx.module).toBeUndefined();
    expect(ctx.exports).toBeUndefined();
    expect(ctx.require).toBeUndefined();
  });

  it("calls install() with window.__nteract for self-registration", () => {
    const mock = createMockNteract();

    const pluginCode = `
      exports.install = function(ctx) {
        ctx.register(["test/mime"], function TestComponent() {});
      };
    `;
    execWrapped(wrapForMcpApp(pluginCode), mock.api);

    expect(mock.registered.has("test/mime")).toBe(true);
  });

  it("supports registerPattern for version-agnostic MIME matching", () => {
    const mock = createMockNteract();

    const pluginCode = `
      exports.install = function(ctx) {
        ctx.registerPattern(
          function(mime) { return mime.startsWith("application/vnd.vega"); },
          function VegaComponent() {}
        );
      };
    `;
    execWrapped(wrapForMcpApp(pluginCode), mock.api);

    expect(mock.patterns.length).toBe(1);
    expect(mock.patterns[0].test("application/vnd.vegalite.v5+json")).toBe(
      true,
    );
  });

  it("provides React via require shim", () => {
    const mock = createMockNteract();

    const pluginCode = `
      var React = require("react");
      exports.install = function(ctx) {
        ctx.register(["test/react"], function() { return React; });
      };
    `;
    execWrapped(wrapForMcpApp(pluginCode), mock.api);

    expect(mock.registered.has("test/react")).toBe(true);
  });

  it("does nothing if no install export", () => {
    const mock = createMockNteract();
    execWrapped(wrapForMcpApp("exports.notInstall = 1;"), mock.api);

    expect(mock.registered.size).toBe(0);
  });

  it("does not throw for empty code", () => {
    const mock = createMockNteract();
    expect(() => execWrapped(wrapForMcpApp(""), mock.api)).not.toThrow();
  });
});

describe("wrapForMcpApp with real plugin bundles", () => {
  const pluginsDir = path.resolve(
    import.meta.dirname,
    "../../../../../crates/runt-mcp/assets/plugins",
  );

  const pluginFiles = ["plotly.js", "vega.js", "leaflet.js"];

  for (const file of pluginFiles) {
    const pluginPath = path.join(pluginsDir, file);

    it(`wraps ${file} without syntax errors`, () => {
      if (!fs.existsSync(pluginPath)) {
        console.warn(`Skipping ${file} — not built (run pnpm build:plugins)`);
        return;
      }

      const code = fs.readFileSync(pluginPath, "utf8");
      const wrapped = wrapForMcpApp(code);

      // Verify the wrapped code is valid JavaScript by compiling it
      // (don't execute — plugins have DOM/browser dependencies)
      expect(() => new vm.Script(wrapped)).not.toThrow();
    });
  }
});
