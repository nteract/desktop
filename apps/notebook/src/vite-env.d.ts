/// <reference types="vite/client" />

declare module "virtual:isolated-renderer" {
  export const rendererCode: string;
  export const rendererCss: string;
}

declare module "virtual:renderer-plugin/markdown" {
  export const code: string;
  export const css: string;
}

declare module "virtual:renderer-plugin/vega" {
  export const code: string;
  export const css: string;
}

declare module "virtual:renderer-plugin/plotly" {
  export const code: string;
  export const css: string;
}
