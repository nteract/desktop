/**
 * `createBrowserHost()` — Browser implementation of `NotebookHost`.
 *
 * Designed for the live-viewer and any future browser-served nteract app.
 * The transport is passed in (typically a WebSocketTransport). Host
 * namespaces that don't apply to browser contexts (updater, dialog, etc.)
 * are implemented as no-ops.
 */

import type { NotebookTransport } from "runtimed";
import { createCommandRegistry } from "../commands";
import type {
  DaemonInfo,
  DaemonProgressPayload,
  DaemonReadyPayload,
  DaemonUnavailablePayload,
  GitInfo,
  HostBlobs,
  HostDaemon,
  HostDaemonEvents,
  HostDeps,
  HostDialog,
  HostExternalLinks,
  HostLog,
  HostNotebook,
  HostRelay,
  HostSystem,
  HostTrust,
  HostUpdater,
  HostWindow,
  NotebookHost,
  TrustInfo,
  TyposquatWarning,
  Unlisten,
} from "../types";

export interface CreateBrowserHostOptions {
  transport: NotebookTransport;
  blobPort?: number;
}

const noop: Unlisten = () => {};

export function createBrowserHost({ transport, blobPort }: CreateBrowserHostOptions): NotebookHost {
  const daemon: HostDaemon = {
    async isConnected() {
      return transport.connected;
    },
    async reconnect() {},
    async getInfo(): Promise<DaemonInfo | null> {
      return null;
    },
    async getReadyInfo(): Promise<DaemonReadyPayload | null> {
      return null;
    },
  };

  const daemonEvents: HostDaemonEvents = {
    onReady(_cb: (payload: DaemonReadyPayload) => void): Unlisten {
      return noop;
    },
    onProgress(_cb: (payload: DaemonProgressPayload) => void): Unlisten {
      return noop;
    },
    onDisconnected(_cb: () => void): Unlisten {
      return noop;
    },
    onUnavailable(_cb: (payload: DaemonUnavailablePayload) => void): Unlisten {
      return noop;
    },
  };

  const relay: HostRelay = {
    async notifySyncReady() {},
  };

  const blobs: HostBlobs = {
    async port() {
      return blobPort ?? 0;
    },
  };

  const trust: HostTrust = {
    async verify(): Promise<TrustInfo> {
      return {
        status: "no_dependencies",
        uv_dependencies: [],
        conda_dependencies: [],
        conda_channels: [],
      };
    },
    async approve() {},
  };

  const deps: HostDeps = {
    async checkTyposquats(_packages: string[]): Promise<TyposquatWarning[]> {
      return [];
    },
  };

  const notebook: HostNotebook = {
    async applyPathChanged(_path: string) {},
    async markClean() {},
  };

  const window_: HostWindow = {
    async getTitle() {
      return document.title;
    },
    async setTitle(title: string) {
      document.title = title;
    },
    onFocusChange(cb: (focused: boolean) => void): Unlisten {
      const onFocus = () => cb(true);
      const onBlur = () => cb(false);
      globalThis.addEventListener("focus", onFocus);
      globalThis.addEventListener("blur", onBlur);
      return () => {
        globalThis.removeEventListener("focus", onFocus);
        globalThis.removeEventListener("blur", onBlur);
      };
    },
  };

  const system: HostSystem = {
    async getGitInfo(): Promise<GitInfo | null> {
      return null;
    },
    async getUsername() {
      return "browser";
    },
  };

  const dialog: HostDialog = {
    async openFile() {
      return null;
    },
    async saveFile() {
      return null;
    },
  };

  const externalLinks: HostExternalLinks = {
    async open(url: string) {
      globalThis.open(url, "_blank");
    },
  };

  const updater: HostUpdater = {
    async check() {
      return null;
    },
  };

  const log: HostLog = {
    debug: console.debug.bind(console),
    info: console.info.bind(console),
    warn: console.warn.bind(console),
    error: console.error.bind(console),
  };

  const commands = createCommandRegistry();

  return {
    name: "browser",
    transport,
    daemon,
    daemonEvents,
    relay,
    blobs,
    trust,
    deps,
    notebook,
    window: window_,
    system,
    dialog,
    externalLinks,
    updater,
    commands,
    log,
  };
}
