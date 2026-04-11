/**
 * React context provider for shared media rendering configuration.
 *
 * MediaProvider supplies renderers and priority settings to all
 * nested MediaRouter instances. This lets you configure MIME type rendering
 * once at the top of the tree — for example, injecting a widget renderer
 * for `application/vnd.jupyter.widget-view+json` — and have all output
 * components inherit it automatically.
 *
 * @example
 * ```tsx
 * <MediaProvider
 *   renderers={{
 *     "application/vnd.jupyter.widget-view+json": ({ data }) => {
 *       const { model_id } = data as { model_id: string };
 *       return <WidgetView modelId={model_id} />;
 *     },
 *   }}
 * >
 *   <OutputArea outputs={cell.outputs} />
 * </MediaProvider>
 * ```
 */

import { createContext, type ReactNode, useContext } from "react";
import type { CustomRenderer } from "./media-router";
import { DEFAULT_PRIORITY } from "./media-router";

interface MediaProviderValue {
  renderers: Record<string, CustomRenderer>;
  priority: readonly string[];
}

interface MediaProviderProps {
  children: ReactNode;
  /**
   * Custom MIME type renderers, inherited by all nested MediaRouter instances.
   * Inner providers merge with outer providers (inner wins for same MIME type).
   */
  renderers?: Record<string, CustomRenderer>;
  /**
   * MIME type priority order, inherited by all nested MediaRouter instances.
   * Overrides the parent provider's priority entirely if provided.
   */
  priority?: readonly string[];
}

const MediaContext = createContext<MediaProviderValue | null>(null);

/**
 * Provider component for shared media rendering configuration.
 *
 * Supports nesting: inner providers merge renderers with outer providers
 * (inner wins for same MIME type), and override priority entirely.
 *
 * Without a provider, MediaRouter uses its built-in defaults.
 */
export function MediaProvider({ children, renderers = {}, priority }: MediaProviderProps) {
  const parent = useContext(MediaContext);

  const value: MediaProviderValue = {
    // Merge renderers: child overrides parent for same MIME type
    renderers: { ...parent?.renderers, ...renderers },
    // Priority: use provided, else inherit from parent, else default
    priority: priority ?? parent?.priority ?? DEFAULT_PRIORITY,
  };

  return <MediaContext.Provider value={value}>{children}</MediaContext.Provider>;
}

/**
 * Access the media rendering context.
 * Returns null if used outside of MediaProvider.
 */
export function useMediaContext(): MediaProviderValue | null {
  return useContext(MediaContext);
}
