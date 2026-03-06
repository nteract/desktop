/**
 * Feature flags for gradual rollout of new functionality.
 *
 * Toggle via browser devtools:
 *   localStorage.setItem("USE_AUTOMERGE_FRONTEND", "true")
 * then reload the page.
 *
 * For E2E tests, use the URL parameter: ?automerge=true
 */
const params = new URLSearchParams(window.location.search);
export const USE_AUTOMERGE_FRONTEND =
  localStorage.getItem("USE_AUTOMERGE_FRONTEND") === "true" ||
  params.get("automerge") === "true";
