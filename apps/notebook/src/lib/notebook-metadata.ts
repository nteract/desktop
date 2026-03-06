import { invoke } from "@tauri-apps/api/core";
import { useMemo, useSyncExternalStore } from "react";
import type { NotebookHandle } from "../wasm/runtimed-wasm/runtimed_wasm.js";
import { logger } from "./logger";

// ---------------------------------------------------------------------------
// Reactive metadata store backed by the WASM Automerge document.
//
// useAutomergeNotebook owns the WASM NotebookHandle and registers it here.
// React hooks use useSyncExternalStore to subscribe — they re-render
// automatically when the doc changes (bootstrap, sync, writes).
//
// One store per window. Safe because there's exactly one notebook
// (one handle) per window.
// ---------------------------------------------------------------------------

let _handle: NotebookHandle | null = null;
let _snapshotCache: string | null = null;
const _subscribers = new Set<() => void>();

/**
 * Notify all useSyncExternalStore subscribers that the doc changed.
 * Call this after any operation that mutates the Automerge document:
 * - setNotebookHandle (bootstrap / reconnect)
 * - receive_sync_message (incoming daemon sync)
 * - set_metadata (local writes)
 */
export function notifyMetadataChanged(): void {
  _snapshotCache = _handle?.get_metadata_snapshot_json() ?? null;
  for (const cb of _subscribers) cb();
}

/**
 * Register the active NotebookHandle. Called by useAutomergeNotebook
 * after bootstrap and cleared on unmount.
 */
export function setNotebookHandle(handle: NotebookHandle | null): void {
  _handle = handle;
  notifyMetadataChanged();
}

/**
 * Subscribe to metadata changes. Used by useSyncExternalStore.
 */
function subscribe(callback: () => void): () => void {
  _subscribers.add(callback);
  return () => _subscribers.delete(callback);
}

/**
 * Get the current metadata snapshot as a JSON string.
 * Used as the getSnapshot function for useSyncExternalStore.
 * Returns the cached value — only updated when notifyMetadataChanged() fires.
 */
function getSnapshotJson(): string | null {
  // _snapshotCache is always set by notifyMetadataChanged() before
  // any subscriber fires. This lazy init handles the first read
  // before any notification has occurred.
  if (_snapshotCache === null) {
    _snapshotCache = _handle?.get_metadata_snapshot_json() ?? null;
  }
  return _snapshotCache;
}

// ---------------------------------------------------------------------------
// React hooks — reactive metadata reads via useSyncExternalStore.
// ---------------------------------------------------------------------------

/**
 * React hook: subscribe to the full metadata snapshot.
 * Re-renders when the Automerge doc changes (bootstrap, sync, writes).
 */
export function useNotebookMetadata(): NotebookMetadataSnapshot | null {
  const json = useSyncExternalStore(subscribe, getSnapshotJson);
  return useMemo(() => {
    if (!json) return null;
    try {
      return JSON.parse(json) as NotebookMetadataSnapshot;
    } catch {
      return null;
    }
  }, [json]);
}

/**
 * React hook: detect the notebook runtime from metadata.
 * Returns "python", "deno", or null.
 */
export function useDetectRuntime(): "python" | "deno" | null {
  const snapshot = useNotebookMetadata();
  if (!snapshot) return null;

  // Check kernelspec.name first
  if (snapshot.kernelspec) {
    const name = snapshot.kernelspec.name.toLowerCase();
    if (name.includes("deno")) return "deno";
    if (name.includes("python")) return "python";
    // Check kernelspec.language
    if (snapshot.kernelspec.language) {
      const lang = snapshot.kernelspec.language.toLowerCase();
      if (lang === "typescript" || lang === "javascript") return "deno";
      if (lang === "python") return "python";
    }
  }

  // Fall back to language_info.name
  if (snapshot.language_info) {
    const name = snapshot.language_info.name.toLowerCase();
    if (name === "deno" || name === "typescript" || name === "javascript")
      return "deno";
    if (name === "python") return "python";
  }

  // Fall back to runt.deno existing (legacy notebooks without kernelspec)
  if (snapshot.runt.deno) return "deno";

  return null;
}

/**
 * React hook: read UV inline dependencies.
 * Returns a stable object reference (via useMemo) to avoid unnecessary
 * re-renders in consumers that use the result as a dependency or prop.
 */
export function useUvDependencies(): {
  dependencies: string[];
  requiresPython: string | null;
} | null {
  const snapshot = useNotebookMetadata();
  const deps = snapshot?.runt?.uv?.dependencies;
  const requiresPython = snapshot?.runt?.uv?.["requires-python"] ?? null;
  return useMemo(() => {
    if (!deps) return null;
    return { dependencies: deps, requiresPython };
  }, [deps, requiresPython]);
}

/**
 * React hook: read Conda inline dependencies.
 * Returns a stable object reference (via useMemo).
 */
export function useCondaDeps(): {
  dependencies: string[];
  channels: string[];
  python: string | null;
} | null {
  const snapshot = useNotebookMetadata();
  const deps = snapshot?.runt?.conda?.dependencies;
  const channels = snapshot?.runt?.conda?.channels;
  const python = snapshot?.runt?.conda?.python ?? null;
  return useMemo(() => {
    if (!deps || !channels) return null;
    return { dependencies: deps, channels, python };
  }, [deps, channels, python]);
}

/**
 * React hook: read the Deno flexible_npm_imports setting.
 */
export function useDenoFlexibleNpmImports(): boolean | null {
  const snapshot = useNotebookMetadata();
  if (!snapshot?.runt?.deno) return null;
  return snapshot.runt.deno.flexible_npm_imports ?? null;
}

// ---------------------------------------------------------------------------
// TypeScript interface matching the Rust NotebookMetadataSnapshot serde shape.
// Kept in sync with crates/notebook-doc/src/metadata.rs.
// ---------------------------------------------------------------------------

export interface KernelspecSnapshot {
  name: string;
  display_name: string;
  language?: string;
}

export interface LanguageInfoSnapshot {
  name: string;
  version?: string;
}

export interface UvInlineMetadata {
  dependencies: string[];
  "requires-python"?: string;
}

export interface CondaInlineMetadata {
  dependencies: string[];
  channels: string[];
  python?: string;
}

export interface DenoMetadata {
  permissions: string[];
  import_map?: string;
  config?: string;
  flexible_npm_imports?: boolean;
}

export interface RuntMetadata {
  schema_version: string;
  env_id?: string;
  uv?: UvInlineMetadata;
  conda?: CondaInlineMetadata;
  deno?: DenoMetadata;
  trust_signature?: string;
  trust_timestamp?: string;
}

export interface NotebookMetadataSnapshot {
  kernelspec?: KernelspecSnapshot;
  language_info?: LanguageInfoSnapshot;
  runt: RuntMetadata;
}

// ---------------------------------------------------------------------------
// Imperative read — used by write helpers that need the current snapshot.
// Prefer the React hooks (useNotebookMetadata, etc.) for component reads.
// ---------------------------------------------------------------------------

/**
 * Read the full typed metadata snapshot imperatively.
 * Used internally by write helpers. Components should use useNotebookMetadata().
 */
function getMetadataSnapshot(): NotebookMetadataSnapshot | null {
  if (!_handle) return null;
  const json = _handle.get_metadata_snapshot_json();
  if (!json) return null;
  try {
    return JSON.parse(json) as NotebookMetadataSnapshot;
  } catch {
    return null;
  }
}

// ---------------------------------------------------------------------------
// Write functions — mutate the WASM doc and sync to the Tauri relay.
// ---------------------------------------------------------------------------

/**
 * Write a metadata snapshot to the WASM doc and sync to the relay.
 * After this returns, both the WASM doc and the relay's doc have the update.
 */
export async function setMetadataSnapshot(
  snapshot: NotebookMetadataSnapshot,
): Promise<boolean> {
  if (!_handle) return false;
  try {
    const json = JSON.stringify(snapshot);
    _handle.set_metadata("notebook_metadata", json);
    await syncToRelay();
    notifyMetadataChanged();
    return true;
  } catch (e) {
    logger.error("[notebook-metadata] setMetadataSnapshot failed:", e);
    return false;
  }
}

/**
 * Generate a sync message from the WASM doc and send it to the Tauri relay.
 * After the invoke returns, the relay's Automerge doc has the update.
 */
async function syncToRelay(): Promise<void> {
  if (!_handle) return;
  const msg = _handle.generate_sync_message();
  if (msg) {
    await invoke("send_automerge_sync", {
      syncMessage: Array.from(msg),
    });
  }
}

// ---------------------------------------------------------------------------
// Package name extraction for dedup (ported from Rust).
// ---------------------------------------------------------------------------

/**
 * Extract the base package name from a dependency specifier.
 * "pandas>=2.0" → "pandas", "numpy" → "numpy", "requests[security]" → "requests"
 */
function extractPackageName(spec: string): string {
  return spec.split(/[>=<!~[;@\s]/)[0].toLowerCase();
}

// ---------------------------------------------------------------------------
// UV dependency write helpers.
// ---------------------------------------------------------------------------

/**
 * Add a UV dependency, deduplicating by package name (case-insensitive).
 * Returns the updated dependencies list, or null on failure.
 */
export async function addUvDependency(pkg: string): Promise<string[] | null> {
  const snapshot = getMetadataSnapshot();
  if (!snapshot) return null;

  const uv = snapshot.runt.uv ?? { dependencies: [] };
  const name = extractPackageName(pkg);

  // Deduplicate: replace existing entry for the same package
  const filtered = uv.dependencies.filter(
    (d) => extractPackageName(d) !== name,
  );
  filtered.push(pkg);

  snapshot.runt.uv = { ...uv, dependencies: filtered };
  const ok = await setMetadataSnapshot(snapshot);
  return ok ? filtered : null;
}

/**
 * Remove a UV dependency by package name (case-insensitive match).
 * Returns the updated dependencies list, or null on failure.
 */
export async function removeUvDependency(
  pkg: string,
): Promise<string[] | null> {
  const snapshot = getMetadataSnapshot();
  if (!snapshot?.runt?.uv) return null;

  const name = extractPackageName(pkg);
  const filtered = snapshot.runt.uv.dependencies.filter(
    (d) => extractPackageName(d) !== name,
  );

  snapshot.runt.uv = { ...snapshot.runt.uv, dependencies: filtered };
  const ok = await setMetadataSnapshot(snapshot);
  return ok ? filtered : null;
}

/**
 * Clear the UV dependency section entirely.
 */
export async function clearUvSection(): Promise<boolean> {
  const snapshot = getMetadataSnapshot();
  if (!snapshot) return false;

  delete snapshot.runt.uv;
  return setMetadataSnapshot(snapshot);
}

/**
 * Set UV requires-python constraint.
 */
export async function setUvRequiresPython(
  requiresPython: string | null,
): Promise<boolean> {
  const snapshot = getMetadataSnapshot();
  if (!snapshot?.runt?.uv) return false;

  if (requiresPython) {
    snapshot.runt.uv["requires-python"] = requiresPython;
  } else {
    delete snapshot.runt.uv["requires-python"];
  }
  return setMetadataSnapshot(snapshot);
}

// ---------------------------------------------------------------------------
// Conda dependency write helpers.
// ---------------------------------------------------------------------------

/**
 * Add a Conda dependency, deduplicating by package name (case-insensitive).
 */
export async function addCondaDependency(
  pkg: string,
): Promise<string[] | null> {
  const snapshot = getMetadataSnapshot();
  if (!snapshot) return null;

  const conda = snapshot.runt.conda ?? {
    dependencies: [],
    channels: ["conda-forge"],
  };
  const name = extractPackageName(pkg);

  const filtered = conda.dependencies.filter(
    (d) => extractPackageName(d) !== name,
  );
  filtered.push(pkg);

  snapshot.runt.conda = { ...conda, dependencies: filtered };
  const ok = await setMetadataSnapshot(snapshot);
  return ok ? filtered : null;
}

/**
 * Remove a Conda dependency by package name.
 */
export async function removeCondaDependency(
  pkg: string,
): Promise<string[] | null> {
  const snapshot = getMetadataSnapshot();
  if (!snapshot?.runt?.conda) return null;

  const name = extractPackageName(pkg);
  const filtered = snapshot.runt.conda.dependencies.filter(
    (d) => extractPackageName(d) !== name,
  );

  snapshot.runt.conda = { ...snapshot.runt.conda, dependencies: filtered };
  const ok = await setMetadataSnapshot(snapshot);
  return ok ? filtered : null;
}

/**
 * Clear the Conda dependency section entirely.
 */
export async function clearCondaSection(): Promise<boolean> {
  const snapshot = getMetadataSnapshot();
  if (!snapshot) return false;

  delete snapshot.runt.conda;
  return setMetadataSnapshot(snapshot);
}

/**
 * Set Conda channels, preserving other conda fields.
 * Creates the conda section if it doesn't exist yet.
 */
export async function setCondaChannels(channels: string[]): Promise<boolean> {
  const snapshot = getMetadataSnapshot();
  if (!snapshot) return false;

  const conda = snapshot.runt.conda ?? {
    dependencies: [],
    channels: [],
  };
  snapshot.runt.conda = { ...conda, channels };
  return setMetadataSnapshot(snapshot);
}

/**
 * Set Conda python version, preserving other conda fields.
 * Creates the conda section if it doesn't exist yet.
 */
export async function setCondaPython(python: string | null): Promise<boolean> {
  const snapshot = getMetadataSnapshot();
  if (!snapshot) return false;

  const conda = snapshot.runt.conda ?? {
    dependencies: [],
    channels: ["conda-forge"],
  };
  if (python) {
    conda.python = python;
  } else {
    delete conda.python;
  }
  snapshot.runt.conda = conda;
  return setMetadataSnapshot(snapshot);
}

// ---------------------------------------------------------------------------
// Deno config write helpers.
// ---------------------------------------------------------------------------

/**
 * Set the flexible_npm_imports setting for Deno notebooks.
 */
export async function setDenoFlexibleNpmImports(
  enabled: boolean,
): Promise<boolean> {
  const snapshot = getMetadataSnapshot();
  if (!snapshot) return false;

  if (!snapshot.runt.deno) {
    snapshot.runt.deno = { permissions: [], flexible_npm_imports: enabled };
  } else {
    snapshot.runt.deno = {
      ...snapshot.runt.deno,
      flexible_npm_imports: enabled,
    };
  }
  return setMetadataSnapshot(snapshot);
}
