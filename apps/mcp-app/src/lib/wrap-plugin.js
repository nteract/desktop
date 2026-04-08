/**
 * Wrap a CJS renderer plugin in an IIFE for MCP App loading.
 *
 * The wrapper:
 * 1. Creates local module/exports/require (no global pollution)
 * 2. Provides React via window.__nteract.require
 * 3. Auto-calls install() with window.__nteract for self-registration
 *
 * @param {string} code - Raw CJS plugin code from Vite build
 * @returns {string} Wrapped code ready for <script> tag loading
 */
export function wrapForMcpApp(code) {
  return [
    "(function(){",
    "var exports={},module={exports:exports};",
    "var require=window.__nteract.require;",
    code,
    ";var _i=module.exports&&module.exports.install;",
    "if(typeof _i==='function')_i(window.__nteract)",
    "})();",
  ].join("\n");
}
