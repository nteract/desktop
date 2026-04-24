import { NotebookHostProvider } from "@nteract/notebook-host";
import { createTauriHost } from "@nteract/notebook-host/tauri";
import { isTauri } from "@tauri-apps/api/core";
import { attachLogger, LogLevel } from "@tauri-apps/plugin-log";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import "./index.css";
import { IsolatedRendererProvider } from "@/components/isolated/isolated-renderer-context";
import { setErrorBoundarySink } from "@/lib/error-boundary";
import { setBlobPortHost } from "./lib/blob-port";
import { setKernelCompletionHost } from "./lib/kernel-completion";
import { logger, setLoggerHost } from "./lib/logger";
import { setMetadataTransport } from "./lib/notebook-metadata";
import { setOpenUrlHost } from "./lib/open-url";

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
setOpenUrlHost(host);
setKernelCompletionHost(host);
setErrorBoundarySink((error, componentStack) => {
  logger.error(
    "[ErrorBoundary] render error:",
    error,
    "component stack:",
    componentStack ?? "(unavailable)",
  );
});

// Capture original console methods BEFORE wrapping. The Rust-log mirror
// below uses these originals to avoid re-entering the `console.error`
// wrapper (which would immediately feed back into plugin-log → Rust →
// attachLogger → here → plugin-log → …, an infinite loop).
const originalConsoleError = console.error.bind(console);
const originalConsoleWarn = console.warn.bind(console);
const originalConsoleInfo = console.info.bind(console);
const originalConsoleDebug = console.debug.bind(console);
const originalConsoleLog = console.log.bind(console);

// Forward `console.error` into the host logger so it lands in notebook.log in
// packaged / CI builds, not just dev devtools. Specifically: WASM panics
// routed via `console_error_panic_hook` become visible in `e2e-logs/app.log`
// with file:line:message — without this, they disappear in production.
// Preserves the original console behavior so devtools stays unchanged.
console.error = (...args: unknown[]) => {
  originalConsoleError(...args);
  try {
    logger.error(...args);
  } catch {
    // Never let the forwarding path break the original error.
  }
};

// Mirror Rust log entries into devtools during dev so plugin-log output is
// visible alongside frontend logs. Uses `attachLogger` (not `attachConsole`,
// which would route through the wrapped `console.error` above and loop) and
// dispatches to the ORIGINAL console methods. The wrapped forwarder never
// sees these writes, so no feedback cycle.
if (isTauri() && import.meta.env.DEV) {
  attachLogger(({ level, message }) => {
    switch (level) {
      case LogLevel.Trace:
      case LogLevel.Debug:
        originalConsoleDebug(message);
        break;
      case LogLevel.Info:
        originalConsoleInfo(message);
        break;
      case LogLevel.Warn:
        originalConsoleWarn(message);
        break;
      case LogLevel.Error:
        originalConsoleError(message);
        break;
      default:
        originalConsoleLog(message);
    }
  }).catch(() => {
    // Plugin missing / IPC unavailable — dev mirror is a nice-to-have.
  });
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <NotebookHostProvider host={host}>
      <IsolatedRendererProvider loader={loadRendererBundle}>
        <App />
      </IsolatedRendererProvider>
    </NotebookHostProvider>
  </StrictMode>,
);
