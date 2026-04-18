import { describe, expect, it, vi } from "vite-plus/test";
import { createCommandRegistry } from "../src/commands";

describe("createCommandRegistry()", () => {
  it("invokes the registered handler with the correct payload", async () => {
    const registry = createCommandRegistry();
    const saved = vi.fn();
    const inserted = vi.fn();
    registry.register("notebook.save", saved);
    registry.register("notebook.insertCell", inserted);

    await registry.run("notebook.save", undefined);
    await registry.run("notebook.insertCell", { type: "markdown" });

    expect(saved).toHaveBeenCalledTimes(1);
    expect(saved).toHaveBeenCalledWith(undefined);
    expect(inserted).toHaveBeenCalledWith({ type: "markdown" });
  });

  it("awaits async handlers", async () => {
    const registry = createCommandRegistry();
    let done = false;
    registry.register("notebook.clearAllOutputs", async () => {
      await new Promise((r) => setTimeout(r, 10));
      done = true;
    });
    await registry.run("notebook.clearAllOutputs", undefined);
    expect(done).toBe(true);
  });

  it("disposer removes the handler", async () => {
    const registry = createCommandRegistry();
    const handler = vi.fn();
    const dispose = registry.register("notebook.runAll", handler);
    expect(registry.list()).toContain("notebook.runAll");

    dispose();
    expect(registry.list()).not.toContain("notebook.runAll");
    // Spy console.warn so "no handler" doesn't spam
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    await registry.run("notebook.runAll", undefined);
    expect(handler).not.toHaveBeenCalled();
    warnSpy.mockRestore();
  });

  it("throws when the same command is registered twice", () => {
    const registry = createCommandRegistry();
    registry.register("notebook.save", () => {});
    expect(() => registry.register("notebook.save", () => {})).toThrow(/already registered/);
  });

  it("disposer is safe to call twice and only removes its own handler", async () => {
    const registry = createCommandRegistry();
    const first = vi.fn();
    const disposeFirst = registry.register("notebook.save", first);

    // First handler is current — dispose removes it.
    disposeFirst();
    expect(registry.list()).not.toContain("notebook.save");

    // A second handler registers fine after the disposer ran.
    const second = vi.fn();
    registry.register("notebook.save", second);
    // Calling the FIRST disposer again must not remove the second handler.
    disposeFirst();
    expect(registry.list()).toContain("notebook.save");

    await registry.run("notebook.save", undefined);
    expect(second).toHaveBeenCalledTimes(1);
    expect(first).not.toHaveBeenCalled();
  });

  it("run() resolves without throwing when no handler is registered", async () => {
    const registry = createCommandRegistry();
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    await expect(registry.run("notebook.save", undefined)).resolves.toBeUndefined();
    expect(warnSpy).toHaveBeenCalledTimes(1);
    warnSpy.mockRestore();
  });

  it("list() returns the currently registered command ids", () => {
    const registry = createCommandRegistry();
    expect(registry.list()).toEqual([]);

    registry.register("notebook.open", () => {});
    registry.register("updater.check", () => {});
    expect(registry.list().sort()).toEqual(["notebook.open", "updater.check"]);
  });
});
