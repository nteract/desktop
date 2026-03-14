import { connectToDevTools } from "react-devtools-core";
import { resolveReactDevToolsConfig } from "./react-devtools";

declare global {
  interface Window {
    __RUNT_REACT_DEVTOOLS_CONNECTED__?: boolean;
  }
}

const config = resolveReactDevToolsConfig(import.meta.env);

if (
  typeof window !== "undefined" &&
  config.enabled &&
  !window.__RUNT_REACT_DEVTOOLS_CONNECTED__
) {
  window.__RUNT_REACT_DEVTOOLS_CONNECTED__ = true;

  try {
    connectToDevTools({
      host: config.host,
      port: config.port,
      useHttps: config.useHttps,
      retryConnectionDelay: config.retryConnectionDelay,
      isAppActive: () => document.visibilityState !== "hidden",
    });
  } catch (error) {
    window.__RUNT_REACT_DEVTOOLS_CONNECTED__ = false;
    throw error;
  }

  import.meta.hot?.dispose(() => {
    window.__RUNT_REACT_DEVTOOLS_CONNECTED__ = false;
  });
}
