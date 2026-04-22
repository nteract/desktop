#!/usr/bin/env node
// Plugin dispatch wrapper. Shipped as bin/nteract-mcp in each of the
// distribution plugin repos (nteract/claude-plugin, nteract/claude-plugin-nightly).
//
// .mcp.json points at this file with no extension. Node resolves
// process.platform + process.arch to pick the right sibling binary
// and exec's it with the caller's stdio so the MCP transport is
// transparent. Claude Code's MCP loader uses child_process.spawn
// which does NOT hunt for .cmd/.exe variants, so a Node wrapper is
// the only portable single-entry-point shape.
//
// Expected siblings (per assemble-plugin-dist.sh):
//   bin/nteract-mcp-aarch64-apple-darwin
//   bin/nteract-mcp-x86_64-apple-darwin
//   bin/nteract-mcp-x86_64-unknown-linux-gnu
//   bin/nteract-mcp-x86_64-pc-windows-msvc.exe
//
// Edit scripts/plugin-dispatch-wrapper.js in nteract/desktop, not the
// copy in the distribution repo — the distribution copy is overwritten
// on every release.

const { spawnSync } = require("child_process");
const path = require("path");

const TARGETS = {
  "darwin-arm64": "nteract-mcp-aarch64-apple-darwin",
  "darwin-x64": "nteract-mcp-x86_64-apple-darwin",
  "linux-x64": "nteract-mcp-x86_64-unknown-linux-gnu",
  "win32-x64": "nteract-mcp-x86_64-pc-windows-msvc.exe",
};

const key = `${process.platform}-${process.arch}`;
const binary = TARGETS[key];

if (!binary) {
  process.stderr.write(
    `nteract-mcp: no bundled binary for ${key}\n` +
      `supported: ${Object.keys(TARGETS).join(", ")}\n`,
  );
  process.exit(1);
}

const target = path.join(__dirname, binary);
const result = spawnSync(target, process.argv.slice(2), {
  stdio: "inherit",
});

if (result.error) {
  process.stderr.write(`nteract-mcp: failed to spawn ${target}: ${result.error.message}\n`);
  process.exit(1);
}

process.exit(result.status ?? 1);
