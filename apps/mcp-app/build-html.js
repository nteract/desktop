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
<div id="root">Loading...</div>
<script type="module">
${js}
</script>
</body>
</html>`;

writeFileSync("dist/widget.html", html);

// Also copy to the nteract package for bundling
const pkgDir = "../../python/nteract/src/nteract";
try {
  writeFileSync(pkgDir + "/_widget.html", html);
  console.log("Built dist/widget.html + copied to nteract package");
} catch {
  console.log("Built dist/widget.html (nteract package copy skipped)");
}
