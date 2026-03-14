import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import "./index.css";
import { IsolatedRendererProvider } from "@/components/isolated/isolated-renderer-context";

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
  const { rendererCode, rendererCss } = await import(
    "virtual:isolated-renderer"
  );
  return { rendererCode, rendererCss };
};

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <IsolatedRendererProvider loader={loadRendererBundle}>
      <App />
    </IsolatedRendererProvider>
  </StrictMode>,
);
