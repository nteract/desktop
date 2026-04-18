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
  mockUnlisten.mockReset();
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
    expect(received).toEqual([{ runtime: "python" }]);
    unlisten();
    await Promise.resolve();
    expect(mockUnlisten).toHaveBeenCalledTimes(1);
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
