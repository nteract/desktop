/**
 * Feature flags for gradual rollout of new functionality.
 *
 * Toggle via browser devtools:
 *   localStorage.setItem("USE_AUTOMERGE_FRONTEND", "true")
 * then reload the page.
 */
export const USE_AUTOMERGE_FRONTEND =
  localStorage.getItem("USE_AUTOMERGE_FRONTEND") === "true";
