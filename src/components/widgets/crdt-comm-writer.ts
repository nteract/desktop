/**
 * Module-level CRDT comm writer for widget state updates.
 *
 * `WidgetUpdateManager` is instantiated at module load (before the WASM
 * handle exists), so it can't capture the writer in its constructor. The
 * notebook app installs the writer via `setCrdtCommWriter` once the handle
 * is ready; the manager reads it back via `getCrdtCommWriter` on each
 * debounced flush. If the writer is still null, the manager re-queues.
 */

type CrdtCommWriter = (commId: string, patch: Record<string, unknown>) => void;

let writer: CrdtCommWriter | null = null;

export function setCrdtCommWriter(w: CrdtCommWriter | null): void {
  writer = w;
}

export function getCrdtCommWriter(): CrdtCommWriter | null {
  return writer;
}
