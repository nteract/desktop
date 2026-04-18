import { useMemo, useSyncExternalStore } from "react";
import type { NotebookTransport } from "runtimed";
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
let _fingerprint: string | null = null;
const _subscribers = new Set<() => void>();

// Module-level transport reference for the outbound sync helper below.
// Wired at boot by main.tsx via setMetadataTransport(host.transport) so the
// non-React syncToRelay helper doesn't reach for Tauri directly.
let _transport: NotebookTransport | null = null;

/** Install the `NotebookTransport` this module uses for outbound frames. */
export function setMetadataTransport(transport: NotebookTransport | null): void {
  _transport = transport;
}

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
 * Notify subscribers that the Automerge doc may have changed.
 *
 * Uses a fingerprint (cheap JSON string from WASM) to detect whether
 * metadata actually changed. If the fingerprint is identical, this is
 * a no-op — no snapshot deserialization, no subscriber notifications.
 *
 * This is called on every sync batch (including cell-only changes),
 * so the fingerprint gate is critical for avoiding unnecessary work
 * during high-frequency output streaming.
 */
export function notifyMetadataChanged(): void {
  const newFingerprint = _handle?.get_metadata_fingerprint() ?? null;
  if (newFingerprint === _fingerprint) return;
  _fingerprint = newFingerprint;
  _snapshotCache = readSnapshot();
  for (const cb of _subscribers) cb();
}

/**
 * Force-notify all subscribers regardless of fingerprint.
 *
 * Used by setNotebookHandle (bootstrap/reconnect) where the handle
 * itself changed and the old fingerprint is meaningless.
 */
function forceNotifyMetadataChanged(): void {
  _fingerprint = _handle?.get_metadata_fingerprint() ?? null;
  _snapshotCache = readSnapshot();
  for (const cb of _subscribers) cb();
}

/**
 * Register the active NotebookHandle. Called by useAutomergeNotebook
 * after bootstrap and cleared on unmount.
 */
export function setNotebookHandle(handle: NotebookHandle | null): void {
  _handle = handle;
  forceNotifyMetadataChanged();
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
 * React hook: read pixi inline deps from the Automerge doc.
 * Returns null when no pixi section is present.
 */
export function usePixiDeps(): {
  dependencies: string[];
  pypiDependencies: string[];
  channels: string[];
  python: string | null;
} | null {
  const snapshot = useNotebookMetadata();
  const deps = snapshot?.runt?.pixi?.dependencies;
  const pypiDeps = snapshot?.runt?.pixi?.pypi_dependencies;
  const channels = snapshot?.runt?.pixi?.channels;
  const python = snapshot?.runt?.pixi?.python ?? null;
  return useMemo(() => {
    if (!deps || !channels) return null;
    return {
      dependencies: deps,
      pypiDependencies: pypiDeps ?? [],
      channels,
      python,
    };
  }, [deps, pypiDeps, channels, python]);
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

export interface PixiInlineMetadata {
  dependencies: string[];
  pypi_dependencies: string[];
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
  pixi?: PixiInlineMetadata;
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
 * Flush local CRDT changes to the daemon via the Tauri relay pipe.
 * Uses flush_local_changes + cancel_last_flush to prevent the sync
 * state consumption race from #1067.
 */
async function syncToRelay(): Promise<void> {
  if (!_handle) return;
  if (!_transport) {
    // Mutations above returned success; a missing transport silently
    // drops the outbound sync. Log loudly so the misconfiguration is
    // obvious to anyone running tests or wiring a new host.
    logger.warn(
      "[notebook-metadata] syncToRelay: no transport configured — " +
        "call setMetadataTransport(host.transport) at boot",
    );
    return;
  }
  const msg = _handle.flush_local_changes();
  if (msg) {
    try {
      await _transport.sendFrame(frame_types.AUTOMERGE_SYNC, msg);
    } catch {
      _handle.cancel_last_flush();
    }
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
// Pixi dependency write helpers.
// ---------------------------------------------------------------------------

/**
 * Add a Pixi conda dependency, deduplicating by package name.
 */
export async function addPixiDependency(pkg: string): Promise<void> {
  if (!_handle) return;
  _handle.add_pixi_dependency(pkg);
  await syncToRelay();
  notifyMetadataChanged();
}

/**
 * Remove a Pixi conda dependency by package name.
 */
export async function removePixiDependency(pkg: string): Promise<void> {
  if (!_handle) return;
  const removed = _handle.remove_pixi_dependency(pkg);
  if (!removed) return;
  await syncToRelay();
  notifyMetadataChanged();
}

/**
 * Clear the Pixi dependency section entirely.
 */
export async function clearPixiSection(): Promise<void> {
  if (!_handle) return;
  _handle.clear_pixi_section();
  await syncToRelay();
  notifyMetadataChanged();
}

/**
 * Set Pixi channels.
 */
export async function setPixiChannels(channels: string[]): Promise<void> {
  if (!_handle) return;
  _handle.set_pixi_channels(JSON.stringify(channels));
  await syncToRelay();
  notifyMetadataChanged();
}

/**
 * Set Pixi python version.
 */
export async function setPixiPython(python: string | null): Promise<void> {
  if (!_handle) return;
  _handle.set_pixi_python(python ?? undefined);
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
