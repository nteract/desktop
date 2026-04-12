import path from "node:path";
import fs from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { wrapForMcpApp } from "./src/lib/wrap-plugin.js";
import {
  buildAllRendererPlugins,
  RENDERER_PLUGINS,
} from "../../src/build/renderer-plugin-builder.ts";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "../..");
const outDir = path.resolve(repoRoot, "crates/runt-mcp/assets/plugins");

async function main() {
  await fs.mkdir(outDir, { recursive: true });

  const plugins = await buildAllRendererPlugins(RENDERER_PLUGINS);

  for (const { name, code, css } of plugins) {
    const jsPath = path.join(outDir, `${name}.js`);
    const wrapped = wrapForMcpApp(code);
    await fs.writeFile(jsPath, wrapped, "utf8");
    const jsSizeKb = (wrapped.length / 1024).toFixed(1);

    let cssSizeKb = "0.0";
    if (css) {
      const cssPath = path.join(outDir, `${name}.css`);
      await fs.writeFile(cssPath, css, "utf8");
      cssSizeKb = (css.length / 1024).toFixed(1);
    }

    console.log(`${name}: JS ${jsSizeKb} kB${css ? `, CSS ${cssSizeKb} kB` : ""}`);
  }

  console.log(`\nPlugins written to ${outDir}`);
  console.log("Rebuild the daemon (cargo build -p runtimed) to embed them.");
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
