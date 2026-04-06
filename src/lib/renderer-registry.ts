/**
 * Renderer plugin registry — shared lookup for MIME type → React component.
 *
 * The registry is populated by installRendererPlugin() in the isolated
 * renderer and queried by both OutputRenderer and MediaRouter. This module
 * exists so both consumers can access the registry without circular imports.
 *
 * Inside the isolated iframe IIFE, all imports resolve to the same module
 * instance, so the registry state is shared automatically.
 */

import type { ComponentType } from "react";

export interface RendererProps {
  data: unknown;
  metadata?: Record<string, unknown>;
  mimeType: string;
}

const exactMatches = new Map<string, ComponentType<RendererProps>>();
const patternMatchers: Array<{
  test: (mime: string) => boolean;
  component: ComponentType<RendererProps>;
}> = [];

/** Register a component for exact MIME type matches. */
export function registerRenderer(
  mimeTypes: string[],
  component: ComponentType<RendererProps>,
): void {
  for (const mt of mimeTypes) {
    exactMatches.set(mt, component);
  }
}

/** Register a component for pattern-based MIME type matching. */
export function registerRendererPattern(
  test: (mime: string) => boolean,
  component: ComponentType<RendererProps>,
): void {
  patternMatchers.push({ test, component });
}

/** Look up a renderer by exact match first, then pattern matchers. */
export function getRenderer(
  mimeType: string,
): ComponentType<RendererProps> | undefined {
  const exact = exactMatches.get(mimeType);
  if (exact) return exact;
  for (const entry of patternMatchers) {
    if (entry.test(mimeType)) return entry.component;
  }
  return undefined;
}
