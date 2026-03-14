declare module "react-devtools-core" {
  export interface ConnectToDevToolsOptions {
    host?: string;
    port?: number;
    useHttps?: boolean;
    retryConnectionDelay?: number;
    isAppActive?: () => boolean;
  }

  export function connectToDevTools(options?: ConnectToDevToolsOptions): void;
}
