/**
 * Module-level CRDT comm writer for widget state updates.
 *
 * Set by the notebook app when the WASM handle is available.
 * Read by anywidget-view.tsx to write state directly to RuntimeStateDoc
 * instead of going through SendComm request/response.
 */

type CrdtCommWriter = (commId: string, patch: Record<string, unknown>) => void;

let writer: CrdtCommWriter | null = null;

export function setCrdtCommWriter(w: CrdtCommWriter | null): void {
  writer = w;
}

export function getCrdtCommWriter(): CrdtCommWriter | null {
  return writer;
}
