/// <reference types="vite/client" />

// KaTeX CSS type declaration for side-effect imports
declare module "katex/dist/katex.min.css";

// lezer-toml type declaration (package doesn't properly export types)
declare module "lezer-toml" {
  import type { LRParser } from "@lezer/lr";
  export const parser: LRParser;
}
