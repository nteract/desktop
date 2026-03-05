/**
 * Dispatch hook that selects between useNotebook and useAutomergeNotebook
 * based on the USE_AUTOMERGE_FRONTEND feature flag.
 *
 * This wrapper exists because React's rules of hooks prohibit conditional
 * hook calls. Instead, we select the implementation at the module level
 * and always call the same hook.
 */

import { USE_AUTOMERGE_FRONTEND } from "../lib/feature-flags";
import { useAutomergeNotebook } from "./useAutomergeNotebook";
import { useNotebook } from "./useNotebook";

export const useNotebookDispatch = USE_AUTOMERGE_FRONTEND
  ? useAutomergeNotebook
  : useNotebook;
