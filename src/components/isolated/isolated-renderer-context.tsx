import { createContext, type ReactNode, useContext, useEffect, useState } from "react";

interface IsolatedRendererBundle {
  rendererCode: string;
  rendererCss: string;
}

interface IsolatedRendererContextValue {
  rendererCode: string | undefined;
  rendererCss: string | undefined;
  isLoading: boolean;
  error: Error | null;
}

const IsolatedRendererContext = createContext<IsolatedRendererContextValue | null>(null);

interface IsolatedRendererProviderProps {
  children: ReactNode;
  /** Base path to fetch isolated-renderer.js and isolated-renderer.css from */
  basePath?: string;
  /** Custom loader function (e.g., for Vite virtual modules) */
  loader?: () => Promise<IsolatedRendererBundle>;
}

// Module-level cache (shared across all provider instances)
let bundleCache: IsolatedRendererBundle | null = null;
let loadingPromise: Promise<IsolatedRendererBundle> | null = null;

/**
 * Provider for the isolated renderer bundle.
 *
 * Wrap your app (or the part that uses IsolatedFrame) with this provider.
 *
 * @example
 * // Option A: Fetch from a URL path
 * <IsolatedRendererProvider basePath="/isolated">
 *   <App />
 * </IsolatedRendererProvider>
 *
 * @example
 * // Option B: Use Vite virtual module (for Tauri/bundled apps)
 * <IsolatedRendererProvider loader={() => import("virtual:isolated-renderer")}>
 *   <App />
 * </IsolatedRendererProvider>
 */
export function IsolatedRendererProvider({
  children,
  basePath,
  loader,
}: IsolatedRendererProviderProps) {
  const [state, setState] = useState<IsolatedRendererContextValue>(() => ({
    rendererCode: bundleCache?.rendererCode,
    rendererCss: bundleCache?.rendererCss,
    isLoading: !bundleCache,
    error: null,
  }));

  useEffect(() => {
    if (bundleCache) {
      // Already loaded, update state if needed
      if (state.isLoading) {
        setState({
          rendererCode: bundleCache.rendererCode,
          rendererCss: bundleCache.rendererCss,
          isLoading: false,
          error: null,
        });
      }
      return;
    }

    let cancelled = false;

    if (!loadingPromise) {
      if (loader) {
        // Use custom loader (Vite plugin, etc.)
        loadingPromise = loader();
      } else if (basePath) {
        // Fetch from URL
        loadingPromise = Promise.all([
          fetch(`${basePath}/isolated-renderer.js`).then((r) => {
            if (!r.ok) throw new Error(`Failed to fetch renderer JS: ${r.status}`);
            return r.text();
          }),
          fetch(`${basePath}/isolated-renderer.css`).then((r) => {
            if (!r.ok) throw new Error(`Failed to fetch renderer CSS: ${r.status}`);
            return r.text();
          }),
        ]).then(([js, css]) => ({ rendererCode: js, rendererCss: css }));
      } else {
        const error = new Error(
          "IsolatedRendererProvider requires either 'basePath' or 'loader' prop. " +
            "See: https://elements.nteract.io/docs/outputs/isolated-frame#setup",
        );
        setState((s) => ({ ...s, isLoading: false, error }));
        return;
      }
    }

    loadingPromise
      .then((bundle) => {
        bundleCache = bundle;
        if (!cancelled) {
          setState({
            rendererCode: bundle.rendererCode,
            rendererCss: bundle.rendererCss,
            isLoading: false,
            error: null,
          });
        }
      })
      .catch((error) => {
        console.error("[IsolatedRendererProvider] Bundle load failed:", error);
        loadingPromise = null; // Allow retry on next mount
        if (!cancelled) {
          setState((s) => ({ ...s, isLoading: false, error }));
        }
      });

    return () => {
      cancelled = true;
    };
  }, [basePath, loader, state.isLoading]);

  return (
    <IsolatedRendererContext.Provider value={state}>{children}</IsolatedRendererContext.Provider>
  );
}

// Default state when no provider is present (e.g., during SSR)
const NO_PROVIDER_STATE: IsolatedRendererContextValue = {
  rendererCode: undefined,
  rendererCss: undefined,
  isLoading: true,
  error: null,
};

/**
 * Hook to access the isolated renderer bundle.
 *
 * Returns a "loading" state if used outside IsolatedRendererProvider,
 * which allows components to render safely during SSR.
 * In development, logs a warning when no provider is present.
 */
export function useIsolatedRenderer(): IsolatedRendererContextValue {
  const context = useContext(IsolatedRendererContext);
  if (!context) {
    // During SSR or when provider is missing, return a "not ready" state
    // This allows components to render without crashing
    if (process.env.NODE_ENV === "development" && typeof window !== "undefined") {
      console.warn(
        "useIsolatedRenderer: No IsolatedRendererProvider found. " +
          "Wrap your app with <IsolatedRendererProvider>. " +
          "See: https://elements.nteract.io/docs/outputs/isolated-frame#setup",
      );
    }
    return NO_PROVIDER_STATE;
  }
  return context;
}

/**
 * Reset the bundle cache (useful for testing).
 * @internal
 */
export function _resetBundleCache() {
  bundleCache = null;
  loadingPromise = null;
}
