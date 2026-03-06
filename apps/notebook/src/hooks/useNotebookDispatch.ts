import { useAutomergeNotebook } from "./useAutomergeNotebook";
import { useNotebook } from "./useNotebook";

/**
 * Check whether the Automerge frontend feature flag is enabled.
 *
 * Activation (either works):
 *   - `localStorage.setItem("USE_AUTOMERGE_FRONTEND", "true")` + reload
 *   - Navigate with `?automerge=true` in the URL
 */
function isAutomergeEnabled(): boolean {
  // Priority 1: Vite build-time env var (set via VITE_USE_AUTOMERGE=true cargo xtask dev)
  try {
    if (import.meta.env.VITE_USE_AUTOMERGE === "true") {
      return true;
    }
  } catch {
    // import.meta.env may not exist outside Vite
  }
  // Priority 2: localStorage (persistent per-browser toggle)
  try {
    if (localStorage.getItem("USE_AUTOMERGE_FRONTEND") === "true") {
      return true;
    }
  } catch {
    // localStorage may be unavailable in some contexts
  }
  // Priority 3: URL query param (useful for one-off testing)
  try {
    const params = new URLSearchParams(window.location.search);
    if (params.get("automerge") === "true") {
      return true;
    }
  } catch {
    // URL parsing may fail in non-browser contexts
  }
  return false;
}

// Evaluate once at module load so the hook choice is stable across renders.
// Changing the flag requires a page reload.
const USE_AUTOMERGE = isAutomergeEnabled();

/**
 * Notebook state hook that delegates to either the legacy `useNotebook`
 * (RPC + optimistic UI) or the new `useAutomergeNotebook` (local-first
 * WASM Automerge doc) based on a feature flag.
 *
 * The return type is identical for both paths — App.tsx doesn't need to
 * know which implementation is active.
 *
 * `USE_AUTOMERGE` is a module-level constant evaluated once at load time,
 * so the branch is stable across all renders and safe to use with hooks.
 */
export function useNotebookDispatch() {
  if (USE_AUTOMERGE) {
    // biome-ignore lint/correctness/useHookAtTopLevel: USE_AUTOMERGE is a module-level constant — the branch is stable across all renders.
    return useAutomergeNotebook();
  }
  // biome-ignore lint/correctness/useHookAtTopLevel: see above — exactly one hook is always called per render.
  return useNotebook();
}
