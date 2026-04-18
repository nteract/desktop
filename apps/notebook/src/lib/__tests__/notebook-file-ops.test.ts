// @vitest-environment jsdom
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import type { NotebookHost } from "@nteract/notebook-host";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import {
  cloneNotebookFile,
  openNotebookFile,
  saveNotebook,
} from "../notebook-file-ops";

const mockOpenDialog = vi.fn<
  (opts?: { filters?: unknown; defaultPath?: string }) => Promise<string | null>
>();
const mockSaveDialog = vi.fn<
  (opts?: { filters?: unknown; defaultPath?: string }) => Promise<string | null>
>();

const stubHost = {
  dialog: {
    openFile: (opts?: { filters?: unknown; defaultPath?: string }) => mockOpenDialog(opts),
    saveFile: (opts?: { filters?: unknown; defaultPath?: string }) => mockSaveDialog(opts),
  },
} as unknown as NotebookHost;

const mockInvoke = vi.fn();

beforeEach(() => {
  mockIPC((cmd, args) => mockInvoke(cmd, args));
});

afterEach(() => {
  mockInvoke.mockReset();
  mockOpenDialog.mockReset();
  mockSaveDialog.mockReset();
  clearMocks();
});

// ---------------------------------------------------------------------------
// saveNotebook
// ---------------------------------------------------------------------------

describe("saveNotebook", () => {
  const flushSync = vi.fn().mockResolvedValue(undefined);

  afterEach(() => {
    flushSync.mockClear();
  });

  it("saves in place when the notebook already has a path", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "has_notebook_path") return true;
      return undefined;
    });

    const result = await saveNotebook(stubHost, flushSync);

    expect(result).toBe(true);
    expect(flushSync).toHaveBeenCalledTimes(1);
    expect(mockInvoke).toHaveBeenCalledWith(
      "has_notebook_path",
      expect.anything(),
    );
    expect(mockInvoke).toHaveBeenCalledWith("save_notebook", expect.anything());
  });

  it("opens a save dialog for untitled notebooks", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "has_notebook_path") return false;
      if (cmd === "get_default_save_directory") return "/home/user/notebooks";
      return undefined;
    });
    mockSaveDialog.mockResolvedValueOnce(
      "/home/user/notebooks/MyNotebook.ipynb",
    );

    const result = await saveNotebook(stubHost, flushSync);

    expect(result).toBe(true);
    expect(mockSaveDialog).toHaveBeenCalledTimes(1);
    expect(mockInvoke).toHaveBeenCalledWith(
      "save_notebook_as",
      expect.objectContaining({
        path: "/home/user/notebooks/MyNotebook.ipynb",
      }),
    );
  });

  it("returns false when the save dialog is cancelled", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "has_notebook_path") return false;
      if (cmd === "get_default_save_directory") return "/tmp";
      return undefined;
    });
    mockSaveDialog.mockResolvedValueOnce(null);

    const result = await saveNotebook(stubHost, flushSync);

    expect(result).toBe(false);
    // save_notebook_as should NOT be called
    const saveAsCalls = mockInvoke.mock.calls.filter(
      ([cmd]) => cmd === "save_notebook_as",
    );
    expect(saveAsCalls).toHaveLength(0);
  });

  it("returns false and logs on error", async () => {
    mockInvoke.mockRejectedValue(new Error("disk full"));

    const result = await saveNotebook(stubHost, flushSync);

    expect(result).toBe(false);
  });

  it("always flushes sync before checking path", async () => {
    mockInvoke.mockRejectedValue(new Error("fail"));

    await saveNotebook(stubHost, flushSync);

    expect(flushSync).toHaveBeenCalledTimes(1);
  });
});

// ---------------------------------------------------------------------------
// openNotebookFile
// ---------------------------------------------------------------------------

describe("openNotebookFile", () => {
  it("opens the selected file in a new window", async () => {
    mockOpenDialog.mockResolvedValueOnce("/path/to/notebook.ipynb");
    mockInvoke.mockResolvedValue(undefined);

    await openNotebookFile(stubHost);

    expect(mockOpenDialog).toHaveBeenCalledTimes(1);
    expect(mockInvoke).toHaveBeenCalledWith(
      "open_notebook_in_new_window",
      expect.objectContaining({ path: "/path/to/notebook.ipynb" }),
    );
  });

  it("does nothing when the dialog is cancelled", async () => {
    mockOpenDialog.mockResolvedValueOnce(null);

    await openNotebookFile(stubHost);

    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("does not throw on error", async () => {
    mockOpenDialog.mockRejectedValueOnce(new Error("permission denied"));

    // Should not throw — errors are logged internally
    await expect(openNotebookFile(stubHost)).resolves.toBeUndefined();
  });
});

// ---------------------------------------------------------------------------
// cloneNotebookFile
// ---------------------------------------------------------------------------

describe("cloneNotebookFile", () => {
  it("clones to the chosen path and opens in a new window", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "get_default_save_directory") return "/home/user/notebooks";
      return undefined;
    });
    mockSaveDialog.mockResolvedValueOnce("/home/user/notebooks/Clone.ipynb");

    await cloneNotebookFile(stubHost);

    expect(mockInvoke).toHaveBeenCalledWith(
      "clone_notebook_to_path",
      expect.objectContaining({ path: "/home/user/notebooks/Clone.ipynb" }),
    );
    expect(mockInvoke).toHaveBeenCalledWith(
      "open_notebook_in_new_window",
      expect.objectContaining({ path: "/home/user/notebooks/Clone.ipynb" }),
    );
  });

  it("does nothing when the save dialog is cancelled", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "get_default_save_directory") return "/tmp";
      return undefined;
    });
    mockSaveDialog.mockResolvedValueOnce(null);

    await cloneNotebookFile(stubHost);

    const cloneCalls = mockInvoke.mock.calls.filter(
      ([cmd]) => cmd === "clone_notebook_to_path",
    );
    expect(cloneCalls).toHaveLength(0);
  });

  it("does not throw on error", async () => {
    mockInvoke.mockRejectedValue(new Error("clone failed"));

    await expect(cloneNotebookFile(stubHost)).resolves.toBeUndefined();
  });
});
