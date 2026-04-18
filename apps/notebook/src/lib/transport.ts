/**
 * Transport factory — picks the right `NotebookTransport` for the host.
 *
 * Production (Tauri app): `TauriTransport`.
 * Dev harness (Electron): `ElectronTransport`, selected by the presence of
 * `window.electronAPI` set up by the harness preload.
 *
 * See apps/dev-harness-electron/README.md for the security posture: the
 * Electron path reuses the same Unix socket the Tauri app uses and does not
 * open any new network listener.
 */

import type { NotebookTransport } from "runtimed";
import { ElectronTransport, isElectronHarness } from "./electron-transport";
import { TauriTransport } from "./tauri-transport";

export function createTransport(): NotebookTransport {
  if (isElectronHarness()) {
    return new ElectronTransport();
  }
  return new TauriTransport();
}
