// @vitest-environment jsdom
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import type { NotebookHost } from "@nteract/notebook-host";
import type { NotebookTransport } from "runtimed";
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

/**
 * Minimal NotebookTransport stub for the save-path test. `sendRequest` is
 * what `NotebookClient.saveNotebook` calls — the rest of the transport
 * surface is unused in these tests.
 */
const mockSendRequest = vi.fn();
const stubTransport = {
  sendRequest: (req: unknown) => mockSendRequest(req),
  sendFrame: async () => {},
  onFrame: () => () => {},
  connected: true,
  disconnect: () => {},
} as unknown as NotebookTransport;

const stubHost = {
  transport: stubTransport,
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
  mockSendRequest.mockReset();
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

  it("saves in place through the transport when the notebook has a path", async () => {
    mockSendRequest.mockResolvedValueOnce({
      result: "notebook_saved",
      path: "/home/user/notebooks/MyNotebook.ipynb",
    });

    const result = await saveNotebook(stubHost, flushSync, true);

    expect(result).toBe(true);
    expect(flushSync).toHaveBeenCalledTimes(1);
    expect(mockSendRequest).toHaveBeenCalledWith(
      expect.objectContaining({ type: "save_notebook", format_cells: true }),
    );
    // No Tauri round-trip for save-in-place.
    expect(mockInvoke).not.toHaveBeenCalledWith(
      "save_notebook",
      expect.anything(),
    );
  });

  it("opens a save dialog for untitled notebooks", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "get_default_save_directory") return "/home/user/notebooks";
      return undefined;
    });
    mockSaveDialog.mockResolvedValueOnce(
      "/home/user/notebooks/MyNotebook.ipynb",
    );

    const result = await saveNotebook(stubHost, flushSync, false);

    expect(result).toBe(true);
    expect(mockSaveDialog).toHaveBeenCalledTimes(1);
    expect(mockInvoke).toHaveBeenCalledWith(
      "save_notebook_as",
      expect.objectContaining({
        path: "/home/user/notebooks/MyNotebook.ipynb",
      }),
    );
    expect(mockSendRequest).not.toHaveBeenCalled();
  });

  it("returns false when the save dialog is cancelled", async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "get_default_save_directory") return "/tmp";
      return undefined;
    });
    mockSaveDialog.mockResolvedValueOnce(null);

    const result = await saveNotebook(stubHost, flushSync, false);

    expect(result).toBe(false);
    // save_notebook_as should NOT be called
    const saveAsCalls = mockInvoke.mock.calls.filter(
      ([cmd]) => cmd === "save_notebook_as",
    );
    expect(saveAsCalls).toHaveLength(0);
  });

  it("returns false on daemon save errors", async () => {
    mockSendRequest.mockResolvedValueOnce({
      result: "save_error",
      error: { type: "io", message: "disk full" },
    });

    const result = await saveNotebook(stubHost, flushSync, true);

    expect(result).toBe(false);
  });

  it("returns false on transport failure", async () => {
    mockSendRequest.mockRejectedValueOnce(new Error("transport down"));

    const result = await saveNotebook(stubHost, flushSync, true);

    expect(result).toBe(false);
  });

  it("always flushes sync before saving", async () => {
    mockSendRequest.mockRejectedValueOnce(new Error("fail"));

    await saveNotebook(stubHost, flushSync, true);

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
  it("invokes clone_notebook_to_ephemeral once and opens no dialog", async () => {
    mockInvoke.mockResolvedValueOnce("new-uuid-1234");

    await cloneNotebookFile(stubHost);

    const cloneCalls = mockInvoke.mock.calls.filter(
      ([cmd]) => cmd === "clone_notebook_to_ephemeral",
    );
    expect(cloneCalls).toHaveLength(1);

    // No dialog, no save-directory lookup, no legacy path construction.
    expect(mockSaveDialog).not.toHaveBeenCalled();
    expect(
      mockInvoke.mock.calls.filter(
        ([cmd]) => cmd === "get_default_save_directory",
      ),
    ).toHaveLength(0);
    expect(
      mockInvoke.mock.calls.filter(
        ([cmd]) => cmd === "open_notebook_in_new_window",
      ),
    ).toHaveLength(0);
    expect(
      mockInvoke.mock.calls.filter(
        ([cmd]) => cmd === "clone_notebook_to_path",
      ),
    ).toHaveLength(0);
  });

  it("does not throw on error", async () => {
    mockInvoke.mockRejectedValue(new Error("clone failed"));

    await expect(cloneNotebookFile(stubHost)).resolves.toBeUndefined();
  });
});
