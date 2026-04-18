import { NotebookHostProvider } from "@nteract/notebook-host";
import { createTauriHost } from "@nteract/notebook-host/tauri";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import "./index.css";
import { IsolatedRendererProvider } from "@/components/isolated/isolated-renderer-context";
import { setBlobPortHost } from "./lib/blob-port";
import { setLoggerHost } from "./lib/logger";
import { setMetadataTransport } from "./lib/notebook-metadata";

// Register built-in widget components
import "@/components/widgets/controls";
import "@/components/widgets/ipycanvas";

// Preload output components used in main bundle (via MediaRouter).
// Note: markdown-output, html-output, svg-output are isolated-only
// and bundled separately in src/isolated-renderer/ - no need to preload here.
import("@/components/outputs/ansi-output");
import("@/components/outputs/image-output");
import("@/components/outputs/json-output");

// Loader for isolated renderer bundle (uses existing Vite virtual module)
const loadRendererBundle = async () => {
  const { rendererCode, rendererCss } = await import("virtual:isolated-renderer");
  return { rendererCode, rendererCss };
};

// Tauri host is constructed once at boot. Every host-platform side
// effect flows through it (see @nteract/notebook-host types). The
// transport is part of the host and shared by the SyncEngine,
// NotebookClient, and anything else that needs to talk to the daemon.
const host = createTauriHost();
// Module-scope helpers that can't reach for useNotebookHost() — hand them
// the references they need right after the host is constructed.
setMetadataTransport(host.transport);
setBlobPortHost(host);
setLoggerHost(host);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <NotebookHostProvider host={host}>
      <IsolatedRendererProvider loader={loadRendererBundle}>
        <App />
      </IsolatedRendererProvider>
    </NotebookHostProvider>
  </StrictMode>,
);
