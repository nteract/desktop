// Inline the built JS + CSS into a single self-contained HTML file.
import { readFileSync, writeFileSync } from "node:fs";

const rawJs = readFileSync("dist/mcp-app.js", "utf-8");
const css = readFileSync("src/style.css", "utf-8");

// Escape </script> inside the JS so the HTML parser doesn't prematurely
// close the script block (e.g. from Zod regex literals in the MCP SDK).
const js = rawJs.replaceAll("</script>", "<\\/script>");

const html = `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="color-scheme" content="light dark" />
<style>
${css}
</style>
</head>
<body>
<div id="root"></div>
<script type="module">
${js}
</script>
</body>
</html>`;

writeFileSync("dist/output.html", html);

// Copy to the nteract Python package for bundling
const pkgDir = "../../python/nteract/src/nteract";
try {
  writeFileSync(`${pkgDir}/_widget.html`, html);
} catch { /* nteract package dir may not exist */ }

// Copy to the runt-mcp crate for Rust include_str! embedding
const mcpDir = "../../crates/runt-mcp/assets";
try {
  writeFileSync(`${mcpDir}/_output.html`, html);
  console.log("Built dist/output.html + copied to nteract package + runt-mcp assets");
} catch {
  console.log("Built dist/output.html (runt-mcp copy skipped)");
}
