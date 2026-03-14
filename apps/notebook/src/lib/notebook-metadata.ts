import { invoke } from "@tauri-apps/api/core";
import { useMemo, useSyncExternalStore } from "react";
import type { NotebookHandle } from "../wasm/runtimed-wasm/runtimed_wasm.js";
import { frame_types } from "./frame-types";
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
let _snapshotCache: NotebookMetadataSnapshot | null = null;
const _subscribers = new Set<() => void>();

/**
 * Read the current metadata snapshot from the WASM handle as a typed object.
 * Returns null if no handle is set or the WASM method returns a non-object.
 */
function readSnapshot(): NotebookMetadataSnapshot | null {
  const raw = _handle?.get_metadata_snapshot();
  return raw && typeof raw === "object"
    ? (raw as NotebookMetadataSnapshot)
    : null;
}

/**
 * Notify all useSyncExternalStore subscribers that the doc changed.
 * Call this after any operation that mutates the Automerge document:
 * - setNotebookHandle (bootstrap / reconnect)
 * - receive_sync_message (incoming daemon sync)
 * - set_metadata (local writes)
 */
export function notifyMetadataChanged(): void {
  _snapshotCache = readSnapshot();
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
 * Get the current metadata snapshot as a native JS object.
 * Used as the getSnapshot function for useSyncExternalStore.
 * Returns the cached value — only updated when notifyMetadataChanged() fires.
 */
function getSnapshot(): NotebookMetadataSnapshot | null {
  // _snapshotCache is always set by notifyMetadataChanged() before
  // any subscriber fires. This lazy init handles the first read
  // before any notification has occurred.
  if (_snapshotCache === null) {
    _snapshotCache = readSnapshot();
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
  return useSyncExternalStore(subscribe, getSnapshot);
}

/**
 * React hook: detect the notebook runtime from metadata.
 * Returns "python", "deno", or null.
 *
 * Delegates to the canonical Rust implementation via WASM
 * (NotebookMetadataSnapshot::detect_runtime). The useSyncExternalStore
 * subscription ensures React re-renders when metadata changes.
 */
export function useDetectRuntime(): "python" | "deno" | null {
  // Subscribe to metadata changes so we re-render when the doc updates.
  useSyncExternalStore(subscribe, getSnapshot);
  if (!_handle) return null;
  return (_handle.detect_runtime() as "python" | "deno") ?? null;
}

/**
 * React hook: read UV inline dependencies.
 * Returns a stable object reference (via useMemo) to avoid unnecessary
 * re-renders in consumers that use the result as a dependency or prop.
 */
export function useUvDependencies(): {
  dependencies: string[];
  requiresPython: string | null;
  prerelease: string | null;
} | null {
  const snapshot = useNotebookMetadata();
  const deps = snapshot?.runt?.uv?.dependencies;
  const requiresPython = snapshot?.runt?.uv?.["requires-python"] ?? null;
  const prerelease = snapshot?.runt?.uv?.prerelease ?? null;
  return useMemo(() => {
    if (!deps) return null;
    return { dependencies: deps, requiresPython, prerelease };
  }, [deps, requiresPython, prerelease]);
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
  /** UV prerelease strategy: "disallow" | "allow" | "if-necessary" | "explicit" | "if-necessary-or-explicit" */
  prerelease?: string;
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
// Write functions — mutate the WASM doc and sync to the Tauri relay.
//
// Dependency mutations (add/remove/clear) delegate to the canonical Rust
// implementations via WASM (NotebookMetadataSnapshot methods in notebook-doc).
// The WASM handle mutates the local Automerge doc, then we sync + notify.
// ---------------------------------------------------------------------------

/**
 * Write a metadata snapshot to the WASM doc and sync to the daemon.
 * After this returns, the WASM doc has the update and a sync message has been sent to the daemon.
 *
 * Prefer the typed mutation functions below for dependency writes. This is
 * still useful for bulk metadata writes (e.g. import flows).
 */
export async function setMetadataSnapshot(
  snapshot: NotebookMetadataSnapshot,
): Promise<boolean> {
  if (!_handle) return false;
  try {
    // Use native WASM method that writes as native Automerge types
    // (maps, lists, scalars) instead of a JSON string blob. This enables
    // per-field CRDT merging for concurrent metadata edits.
    _handle.set_metadata_snapshot_value(snapshot);
    await syncToRelay();
    notifyMetadataChanged();
    return true;
  } catch (e) {
    logger.error("[notebook-metadata] setMetadataSnapshot failed:", e);
    return false;
  }
}

/**
 * Generate a sync message from the WASM doc and send it to the daemon via the Tauri relay pipe.
 */
async function syncToRelay(): Promise<void> {
  if (!_handle) return;
  const msg = _handle.generate_sync_message();
  if (msg) {
    const frameData = new Uint8Array(1 + msg.length);
    frameData[0] = frame_types.AUTOMERGE_SYNC;
    frameData.set(msg, 1);
    await invoke("send_frame", {
      frameData: Array.from(frameData),
    });
  }
}

// ---------------------------------------------------------------------------
// UV dependency write helpers.
//
// These delegate to the canonical Rust implementations in notebook-doc via
// WASM. Dedup, case-insensitive matching, and field preservation are handled
// in Rust — the TS layer just calls the WASM method, syncs, and notifies.
// ---------------------------------------------------------------------------

/**
 * Add a UV dependency, deduplicating by package name (case-insensitive).
 */
export async function addUvDependency(pkg: string): Promise<void> {
  if (!_handle) return;
  _handle.add_uv_dependency(pkg);
  await syncToRelay();
  notifyMetadataChanged();
}

/**
 * Remove a UV dependency by package name (case-insensitive match).
 */
export async function removeUvDependency(pkg: string): Promise<void> {
  if (!_handle) return;
  const removed = _handle.remove_uv_dependency(pkg);
  if (!removed) return;
  await syncToRelay();
  notifyMetadataChanged();
}

/**
 * Clear the UV dependency section entirely.
 */
export async function clearUvSection(): Promise<void> {
  if (!_handle) return;
  _handle.clear_uv_section();
  await syncToRelay();
  notifyMetadataChanged();
}

/**
 * Set UV requires-python constraint.
 */
export async function setUvRequiresPython(
  requiresPython: string | null,
): Promise<void> {
  if (!_handle) return;
  _handle.set_uv_requires_python(requiresPython ?? undefined);
  await syncToRelay();
  notifyMetadataChanged();
}

/**
 * Set UV prerelease strategy.
 * Pass "allow", "disallow", "if-necessary", "explicit", "if-necessary-or-explicit", or null to clear.
 */
export async function setUvPrerelease(
  prerelease: string | null,
): Promise<void> {
  if (!_handle) return;
  _handle.set_uv_prerelease(prerelease ?? undefined);
  await syncToRelay();
  notifyMetadataChanged();
}

// ---------------------------------------------------------------------------
// Conda dependency write helpers.
// ---------------------------------------------------------------------------

/**
 * Add a Conda dependency, deduplicating by package name (case-insensitive).
 */
export async function addCondaDependency(pkg: string): Promise<void> {
  if (!_handle) return;
  _handle.add_conda_dependency(pkg);
  await syncToRelay();
  notifyMetadataChanged();
}

/**
 * Remove a Conda dependency by package name.
 */
export async function removeCondaDependency(pkg: string): Promise<void> {
  if (!_handle) return;
  const removed = _handle.remove_conda_dependency(pkg);
  if (!removed) return;
  await syncToRelay();
  notifyMetadataChanged();
}

/**
 * Clear the Conda dependency section entirely.
 */
export async function clearCondaSection(): Promise<void> {
  if (!_handle) return;
  _handle.clear_conda_section();
  await syncToRelay();
  notifyMetadataChanged();
}

/**
 * Set Conda channels, preserving other conda fields.
 * Creates the conda section if it doesn't exist yet.
 */
export async function setCondaChannels(channels: string[]): Promise<void> {
  if (!_handle) return;
  _handle.set_conda_channels(JSON.stringify(channels));
  await syncToRelay();
  notifyMetadataChanged();
}

/**
 * Set Conda python version, preserving other conda fields.
 * Creates the conda section if it doesn't exist yet.
 */
export async function setCondaPython(python: string | null): Promise<void> {
  if (!_handle) return;
  _handle.set_conda_python(python ?? undefined);
  await syncToRelay();
  notifyMetadataChanged();
}

// ---------------------------------------------------------------------------
// Deno config write helpers.
//
// setDenoFlexibleNpmImports still uses the bulk setMetadataSnapshot path
// since there's no dedicated WASM method for it yet.
// ---------------------------------------------------------------------------

/**
 * Set the flexible_npm_imports setting for Deno notebooks.
 */
export async function setDenoFlexibleNpmImports(
  enabled: boolean,
): Promise<boolean> {
  if (!_handle) return false;
  const snapshot = readSnapshot();
  if (!snapshot) return false;
  try {
    if (!snapshot.runt.deno) {
      snapshot.runt.deno = { permissions: [], flexible_npm_imports: enabled };
    } else {
      snapshot.runt.deno = {
        ...snapshot.runt.deno,
        flexible_npm_imports: enabled,
      };
    }
    return setMetadataSnapshot(snapshot);
  } catch {
    return false;
  }
}
