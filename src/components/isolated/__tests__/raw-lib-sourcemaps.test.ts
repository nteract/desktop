/**
 * Verify that raw library imports have sourceMappingURL comments stripped.
 *
 * When libraries are eval()'d inside sandboxed iframes, the browser attempts
 * to fetch .map files referenced by sourceMappingURL comments. Since the
 * iframes use blob: URLs, these requests fail with 404s and leak network
 * traffic. The rawLibPlugin strips these comments at build time. See #1464.
 */

import { describe, expect, it } from "vite-plus/test";

describe("raw library sourcemap stripping", () => {
  it("vega libraries do not contain sourceMappingURL", async () => {
    const [vegaMod, vegaLiteMod, vegaEmbedMod] = await Promise.all([
      import("vega-raw"),
      import("vega-lite-raw"),
      import("vega-embed-raw"),
    ]);

    for (const mod of [vegaMod, vegaLiteMod, vegaEmbedMod]) {
      expect(mod.default).not.toMatch(/sourceMappingURL/);
    }
  });

  it("plotly does not contain sourceMappingURL", async () => {
    const mod = await import("plotly-raw");
    expect(mod.default).not.toMatch(/sourceMappingURL/);
  });

  it("leaflet does not contain sourceMappingURL", async () => {
    const [jsMod, cssMod] = await Promise.all([
      import("leaflet-js-raw"),
      import("leaflet-css-raw"),
    ]);

    for (const mod of [jsMod, cssMod]) {
      expect(mod.default).not.toMatch(/sourceMappingURL/);
    }
  });
});
