/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_REACT_DEVTOOLS_DISABLED?: string;
  readonly VITE_REACT_DEVTOOLS_HOST?: string;
  readonly VITE_REACT_DEVTOOLS_PORT?: string;
  readonly VITE_REACT_DEVTOOLS_HTTPS?: string;
  readonly VITE_REACT_DEVTOOLS_RETRY_DELAY_MS?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}

declare module "virtual:isolated-renderer" {
  export const rendererCode: string;
  export const rendererCss: string;
}
