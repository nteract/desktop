import "./index.css";
if (import.meta.env.DEV) {
  await import("./lib/connect-react-devtools");
}

const [
  { StrictMode },
  { createRoot },
  { default: App },
  { IsolatedRendererProvider },
] = await Promise.all([
  import("react"),
  import("react-dom/client"),
  import("./App"),
  import("@/components/isolated/isolated-renderer-context"),
]);

// Register built-in widget components.
void import("@/components/widgets/controls");
void import("@/components/widgets/ipycanvas");

// Preload output components used in the main bundle (via MediaRouter).
// markdown/html/svg outputs stay isolated-only in the iframe bundle.
void import("@/components/outputs/ansi-output");
void import("@/components/outputs/image-output");
void import("@/components/outputs/json-output");

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
