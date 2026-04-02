/// <reference types="vite/client" />

// KaTeX CSS type declaration for side-effect imports
declare module "katex/dist/katex.min.css";

// Vega UMD builds loaded as raw strings via vegaRawPlugin (see vite.config.ts).
// These virtual modules bypass restrictive "exports" fields in vega v6+ packages.
declare module "vega-raw" {
  const content: string;
  export default content;
}
declare module "vega-lite-raw" {
  const content: string;
  export default content;
}
declare module "vega-embed-raw" {
  const content: string;
  export default content;
}

// Leaflet JS and CSS loaded as raw strings via vegaRawPlugin (see vite.config.ts).
declare module "leaflet-js-raw" {
  const content: string;
  export default content;
}
declare module "leaflet-css-raw" {
  const content: string;
  export default content;
}

// lezer-toml type declaration (package doesn't properly export types)
declare module "lezer-toml" {
  import type { LRParser } from "@lezer/lr";
  export const parser: LRParser;
}
