// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const capturedInvokes: Array<{ cmd: string; args: unknown }> = [];
const capturedListens: Array<{ event: string; cb: (ev: { payload: unknown }) => void }> = [];
const mockUnlisten = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn((cmd: string, args?: unknown) => {
    capturedInvokes.push({ cmd, args });
    // Shape-of-return for the commands the tests hit:
    switch (cmd) {
      case "is_daemon_connected":
        return Promise.resolve(true);
      case "get_git_info":
        return Promise.resolve({ branch: "main", commit: "abc", description: null });
      case "get_daemon_info":
        return Promise.resolve({
          version: "2.2.0",
          socket_path: "/tmp/sock",
          is_dev_mode: true,
        });
      case "get_blob_port":
        return Promise.resolve(12345);
      case "verify_notebook_trust":
        return Promise.resolve({
          status: "trusted",
          uv_dependencies: [],
          conda_dependencies: [],
          conda_channels: [],
        });
      case "check_typosquats":
        return Promise.resolve([]);
      case "get_username":
        return Promise.resolve("kyle");
      case "get_daemon_ready_info":
        return Promise.resolve({
          notebook_id: "nb-1",
          cell_count: 2,
          needs_trust_approval: false,
          ephemeral: true,
          notebook_path: null,
          runtime: "python",
        });
      default:
        return Promise.resolve(undefined);
    }
  }),
  isTauri: () => false,
}));

vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    listen: vi.fn(async (event: string, cb: (ev: { payload: unknown }) => void) => {
      capturedListens.push({ event, cb });
      return mockUnlisten;
    }),
    setZoom: vi.fn(),
  }),
}));

const mockWindowUnlisten = vi.fn();
let capturedFocusCb: ((ev: { payload: boolean }) => void) | null = null;
let mockWindowTitle = "notebook";

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    title: vi.fn(async () => mockWindowTitle),
    setTitle: vi.fn(async (t: string) => {
      mockWindowTitle = t;
    }),
    onFocusChanged: vi.fn(async (cb: (ev: { payload: boolean }) => void) => {
      capturedFocusCb = cb;
      return mockWindowUnlisten;
    }),
  }),
}));

const pluginLogCalls: Array<{ level: string; message: string }> = [];

vi.mock("@tauri-apps/plugin-log", () => ({
  attachConsole: vi.fn(async () => () => {}),
  debug: vi.fn(async (message: string) => {
    pluginLogCalls.push({ level: "debug", message });
  }),
  info: vi.fn(async (message: string) => {
    pluginLogCalls.push({ level: "info", message });
  }),
  warn: vi.fn(async (message: string) => {
    pluginLogCalls.push({ level: "warn", message });
  }),
  error: vi.fn(async (message: string) => {
    pluginLogCalls.push({ level: "error", message });
  }),
}));

const capturedDialogCalls: Array<{ kind: "open" | "save"; opts: unknown }> = [];
let openDialogResult: string | string[] | null = null;
let saveDialogResult: string | null = null;

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(async (opts?: unknown) => {
    capturedDialogCalls.push({ kind: "open", opts });
    return openDialogResult;
  }),
  save: vi.fn(async (opts?: unknown) => {
    capturedDialogCalls.push({ kind: "save", opts });
    return saveDialogResult;
  }),
}));

const capturedShellOpens: string[] = [];

vi.mock("@tauri-apps/plugin-shell", () => ({
  open: vi.fn(async (url: string) => {
    capturedShellOpens.push(url);
  }),
}));

let updateCheckResult: { version: string } | null = null;

vi.mock("@tauri-apps/plugin-updater", () => ({
  check: vi.fn(async () => updateCheckResult),
}));

import type { NotebookTransport } from "runtimed";
import { createTauriHost } from "../src/tauri";

/** Minimal NotebookTransport double — just enough to satisfy the type. */
const stubTransport: NotebookTransport = {
  connected: true,
  sendFrame: vi.fn(),
  onFrame: vi.fn(() => () => {}),
  sendRequest: vi.fn(),
  disconnect: vi.fn(),
};

beforeEach(() => {
  capturedInvokes.length = 0;
  capturedListens.length = 0;
  pluginLogCalls.length = 0;
  capturedDialogCalls.length = 0;
  capturedShellOpens.length = 0;
  openDialogResult = null;
  saveDialogResult = null;
  updateCheckResult = null;
  mockUnlisten.mockReset();
  mockWindowUnlisten.mockReset();
  capturedFocusCb = null;
  mockWindowTitle = "notebook";
});

describe("createTauriHost()", () => {
  it("exposes the transport instance unchanged", () => {
    const host = createTauriHost({ transport: stubTransport });
    expect(host.transport).toBe(stubTransport);
    expect(host.name).toBe("tauri");
  });

  it("routes daemon.isConnected to is_daemon_connected", async () => {
    const host = createTauriHost({ transport: stubTransport });
    await expect(host.daemon.isConnected()).resolves.toBe(true);
    expect(capturedInvokes.at(-1)?.cmd).toBe("is_daemon_connected");
  });

  it("routes daemon.reconnect to reconnect_to_daemon", async () => {
    const host = createTauriHost({ transport: stubTransport });
    await host.daemon.reconnect();
    expect(capturedInvokes.at(-1)?.cmd).toBe("reconnect_to_daemon");
  });

  it("routes daemon.getInfo to get_daemon_info and passes the payload through", async () => {
    const host = createTauriHost({ transport: stubTransport });
    const info = await host.daemon.getInfo();
    expect(info).toEqual({
      version: "2.2.0",
      socket_path: "/tmp/sock",
      is_dev_mode: true,
    });
    expect(capturedInvokes.at(-1)?.cmd).toBe("get_daemon_info");
  });

  it("routes daemon.getReadyInfo to get_daemon_ready_info and passes ephemeral + path through", async () => {
    const host = createTauriHost({ transport: stubTransport });
    const info = await host.daemon.getReadyInfo();
    expect(capturedInvokes.at(-1)?.cmd).toBe("get_daemon_ready_info");
    expect(info?.ephemeral).toBe(true);
    expect(info?.notebook_path).toBe(null);
    expect(info?.runtime).toBe("python");
  });

  it("routes blobs.port to get_blob_port", async () => {
    const host = createTauriHost({ transport: stubTransport });
    await expect(host.blobs.port()).resolves.toBe(12345);
    expect(capturedInvokes.at(-1)?.cmd).toBe("get_blob_port");
  });

  it("routes trust.verify / approve to the correct commands", async () => {
    const host = createTauriHost({ transport: stubTransport });
    const verify = await host.trust.verify();
    expect(verify.status).toBe("trusted");
    await host.trust.approve();
    const cmds = capturedInvokes.map((x) => x.cmd);
    expect(cmds).toEqual(
      expect.arrayContaining(["verify_notebook_trust", "approve_notebook_trust"]),
    );
  });

  it("routes deps.checkTyposquats to check_typosquats (not trust)", async () => {
    const host = createTauriHost({ transport: stubTransport });
    await host.deps.checkTyposquats(["requestz"]);
    const typosquat = capturedInvokes.find((x) => x.cmd === "check_typosquats");
    expect(typosquat?.args).toEqual({ packages: ["requestz"] });
  });

  it("routes notebook.markClean / applyPathChanged to the correct commands", async () => {
    const host = createTauriHost({ transport: stubTransport });
    await host.notebook.markClean();
    await host.notebook.applyPathChanged("/tmp/nb.ipynb");
    expect(capturedInvokes.map((x) => x.cmd)).toEqual([
      "mark_notebook_clean",
      "apply_path_changed",
    ]);
    expect(capturedInvokes[1].args).toEqual({ path: "/tmp/nb.ipynb" });
  });

  it("system.getGitInfo and getUsername route to the correct commands", async () => {
    const host = createTauriHost({ transport: stubTransport });
    await expect(host.system.getGitInfo()).resolves.toEqual({
      branch: "main",
      commit: "abc",
      description: null,
    });
    await expect(host.system.getUsername()).resolves.toBe("kyle");
  });

  it("daemonEvents.onReady subscribes to 'daemon:ready' and returns a working unlisten", async () => {
    const host = createTauriHost({ transport: stubTransport });
    // Reset the unlisten mock after construction — the menu bridge wires up
    // many listeners whose disposers also share mockUnlisten. We only care
    // about the daemon-ready listener this test installed.
    mockUnlisten.mockClear();
    const received: unknown[] = [];
    const unlisten = host.daemonEvents.onReady((p) => received.push(p));
    // Flush the listen() promise so the callback is registered.
    await Promise.resolve();
    const entry = capturedListens.find((x) => x.event === "daemon:ready");
    expect(entry).toBeTruthy();
    entry?.cb({ payload: { runtime: "python" } });
    expect(received).toContainEqual({ runtime: "python" });
    unlisten();
    await Promise.resolve();
    expect(mockUnlisten).toHaveBeenCalledTimes(1);
  });

  it("daemonEvents.onReady also backfills cached payload from get_daemon_ready_info", async () => {
    const host = createTauriHost({ transport: stubTransport });
    const received: unknown[] = [];
    host.daemonEvents.onReady((p) => received.push(p));
    // Let both the listen() promise and the get_daemon_ready_info invoke
    // settle. Two awaits covers the two-step promise chain inside onReady.
    await Promise.resolve();
    await Promise.resolve();
    // Mock returns the canned ready info (ephemeral=true, runtime=python).
    expect(received).toContainEqual({
      notebook_id: "nb-1",
      cell_count: 2,
      needs_trust_approval: false,
      ephemeral: true,
      notebook_path: null,
      runtime: "python",
    });
  });

  it("daemonEvents.onReady drops the cached backfill when unlistened before resolution", async () => {
    const host = createTauriHost({ transport: stubTransport });
    const received: unknown[] = [];
    const unlisten = host.daemonEvents.onReady((p) => received.push(p));
    // Unlisten BEFORE any promises resolve. StrictMode's double-mount
    // dispose exercises exactly this timing.
    unlisten();
    await Promise.resolve();
    await Promise.resolve();
    expect(received).toEqual([]);
  });

  it("daemonEvents.onReady ignores live emissions after unlisten", async () => {
    const host = createTauriHost({ transport: stubTransport });
    const received: unknown[] = [];
    const unlisten = host.daemonEvents.onReady((p) => received.push(p));
    // Let both the cache backfill and listen() promise settle before the
    // interesting part of the test.
    await Promise.resolve();
    await Promise.resolve();
    const entry = capturedListens.find((x) => x.event === "daemon:ready");
    // Reset before testing post-unlisten behavior specifically.
    const countBefore = received.length;
    unlisten();
    // Simulate a late live emission (Tauri-side unlisten hasn't been
    // processed yet). The JS-side wrapper must gate on `cancelled`.
    entry?.cb({ payload: { runtime: "deno" } });
    expect(received.length).toBe(countBefore);
  });

  it("relay.notifySyncReady invokes notify_sync_ready (not on daemonEvents)", async () => {
    const host = createTauriHost({ transport: stubTransport });
    await host.relay.notifySyncReady();
    expect(capturedInvokes.at(-1)?.cmd).toBe("notify_sync_ready");
    // Sanity: subscribe-only namespace shouldn't have the outbound method.
    expect(
      (host.daemonEvents as unknown as { notifySyncReady?: unknown }).notifySyncReady,
    ).toBeUndefined();
  });

  it("daemon.isConnected returns false when invoke rejects", async () => {
    const mod = await import("@tauri-apps/api/core");
    const rejectOnce = vi.spyOn(mod, "invoke").mockRejectedValueOnce(new Error("boom"));
    const host = createTauriHost({ transport: stubTransport });
    await expect(host.daemon.isConnected()).resolves.toBe(false);
    rejectOnce.mockRestore();
  });

  it("exposes a command registry", () => {
    const host = createTauriHost({ transport: stubTransport });
    expect(host.commands).toBeTruthy();
    expect(typeof host.commands.register).toBe("function");
    expect(typeof host.commands.run).toBe("function");
  });

  it("menu bridge subscribes to every known menu:* event", () => {
    createTauriHost({ transport: stubTransport });
    const events = capturedListens.map((x) => x.event);
    // Notebook-scoped commands.
    expect(events).toEqual(
      expect.arrayContaining([
        "menu:save",
        "menu:open",
        "menu:clone",
        "menu:insert-cell",
        "menu:clear-outputs",
        "menu:clear-all-outputs",
        "menu:run-all",
        "menu:restart-and-run-all",
        "menu:check-for-updates",
      ]),
    );
    // Zoom handled host-side (no command id).
    expect(events).toEqual(
      expect.arrayContaining(["menu:zoom-in", "menu:zoom-out", "menu:zoom-reset"]),
    );
  });

  it("menu bridge routes menu:save to host.commands.run('notebook.save')", async () => {
    const host = createTauriHost({ transport: stubTransport });
    const handler = vi.fn();
    host.commands.register("notebook.save", handler);
    const saveEntry = capturedListens.find((x) => x.event === "menu:save");
    expect(saveEntry).toBeTruthy();
    // Flush the listen() promise before dispatching.
    await Promise.resolve();
    saveEntry?.cb({ payload: undefined });
    // Let the async run() call settle.
    await Promise.resolve();
    expect(handler).toHaveBeenCalledTimes(1);
  });

  it("window.getTitle / setTitle route to getCurrentWindow", async () => {
    const host = createTauriHost({ transport: stubTransport });
    await expect(host.window.getTitle()).resolves.toBe("notebook");
    await host.window.setTitle("* notebook");
    await expect(host.window.getTitle()).resolves.toBe("* notebook");
  });

  it("window.onFocusChange forwards focused boolean and returns a working unlisten", async () => {
    const host = createTauriHost({ transport: stubTransport });
    const seen: boolean[] = [];
    const unlisten = host.window.onFocusChange((focused) => seen.push(focused));
    // Flush the onFocusChanged() promise so the cb is registered.
    await Promise.resolve();
    expect(capturedFocusCb).toBeTruthy();
    capturedFocusCb?.({ payload: true });
    capturedFocusCb?.({ payload: false });
    expect(seen).toEqual([true, false]);
    unlisten();
    await Promise.resolve();
    expect(mockWindowUnlisten).toHaveBeenCalledTimes(1);
  });

  it("dialog.openFile routes to plugin-dialog.open with the chosen filters", async () => {
    openDialogResult = "/tmp/a.ipynb";
    const host = createTauriHost({ transport: stubTransport });
    const path = await host.dialog.openFile({
      filters: [{ name: "Jupyter Notebook", extensions: ["ipynb"] }],
    });
    expect(path).toBe("/tmp/a.ipynb");
    expect(capturedDialogCalls).toEqual([
      {
        kind: "open",
        opts: {
          multiple: false,
          filters: [{ name: "Jupyter Notebook", extensions: ["ipynb"] }],
          defaultPath: undefined,
        },
      },
    ]);
  });

  it("dialog.openFile returns null when the plugin returns an array (defensive)", async () => {
    openDialogResult = ["/tmp/a.ipynb"];
    const host = createTauriHost({ transport: stubTransport });
    await expect(host.dialog.openFile()).resolves.toBe(null);
  });

  it("dialog.saveFile routes to plugin-dialog.save and surfaces cancellation as null", async () => {
    saveDialogResult = null;
    const host = createTauriHost({ transport: stubTransport });
    await expect(host.dialog.saveFile({ defaultPath: "/tmp/Untitled.ipynb" })).resolves.toBe(null);
    expect(capturedDialogCalls).toEqual([
      {
        kind: "save",
        opts: { filters: undefined, defaultPath: "/tmp/Untitled.ipynb" },
      },
    ]);
  });

  it("externalLinks.open forwards to plugin-shell.open", async () => {
    const host = createTauriHost({ transport: stubTransport });
    await host.externalLinks.open("https://example.com");
    expect(capturedShellOpens).toEqual(["https://example.com"]);
  });

  it("updater.check returns { version } when plugin-updater reports an update", async () => {
    updateCheckResult = { version: "2.3.0" };
    const host = createTauriHost({ transport: stubTransport });
    await expect(host.updater.check()).resolves.toEqual({ version: "2.3.0" });
  });

  it("updater.check returns null when the app is up to date", async () => {
    updateCheckResult = null;
    const host = createTauriHost({ transport: stubTransport });
    await expect(host.updater.check()).resolves.toBe(null);
  });

  it("host.log forwards each level to plugin-log", () => {
    const host = createTauriHost({ transport: stubTransport });
    host.log.debug("hello");
    host.log.info("world");
    host.log.warn("careful");
    host.log.error("oops");
    expect(pluginLogCalls).toEqual([
      { level: "debug", message: "hello" },
      { level: "info", message: "world" },
      { level: "warn", message: "careful" },
      { level: "error", message: "oops" },
    ]);
  });

  it("menu bridge accepts code/markdown/raw payloads on menu:insert-cell and drops the rest", async () => {
    const host = createTauriHost({ transport: stubTransport });
    const handler = vi.fn();
    host.commands.register("notebook.insertCell", handler);
    const entry = capturedListens.find((x) => x.event === "menu:insert-cell");
    await Promise.resolve();

    entry?.cb({ payload: "markdown" });
    entry?.cb({ payload: "code" });
    entry?.cb({ payload: "raw" });
    await Promise.resolve();
    expect(handler).toHaveBeenCalledTimes(3);
    expect(handler).toHaveBeenNthCalledWith(1, { type: "markdown" });
    expect(handler).toHaveBeenNthCalledWith(2, { type: "code" });
    expect(handler).toHaveBeenNthCalledWith(3, { type: "raw" });

    // Unknown payload is dropped rather than silently coerced to "code".
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    entry?.cb({ payload: "gibberish" });
    await Promise.resolve();
    expect(handler).toHaveBeenCalledTimes(3); // still 3 — the 4th was skipped
    expect(warnSpy).toHaveBeenCalled();
    warnSpy.mockRestore();
  });
});
