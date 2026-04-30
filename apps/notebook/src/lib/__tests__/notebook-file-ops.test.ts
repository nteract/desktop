// @vitest-environment jsdom
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
const mockGetDefaultSaveDirectory = vi.fn<() => Promise<string>>();
const mockSaveAs = vi.fn<(path: string) => Promise<void>>();
const mockOpenInNewWindow = vi.fn<(path: string) => Promise<void>>();
const mockCloneToEphemeral = vi.fn<() => Promise<string>>();
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
  notebook: {
    getDefaultSaveDirectory: () => mockGetDefaultSaveDirectory(),
    saveAs: (path: string) => mockSaveAs(path),
    openInNewWindow: (path: string) => mockOpenInNewWindow(path),
    cloneToEphemeral: () => mockCloneToEphemeral(),
  },
} as unknown as NotebookHost;

beforeEach(() => {
  mockGetDefaultSaveDirectory.mockResolvedValue("/home/user/notebooks");
  mockSaveAs.mockResolvedValue(undefined);
  mockOpenInNewWindow.mockResolvedValue(undefined);
  mockCloneToEphemeral.mockResolvedValue("new-uuid-1234");
});

afterEach(() => {
  mockOpenDialog.mockReset();
  mockSaveDialog.mockReset();
  mockSendRequest.mockReset();
  mockGetDefaultSaveDirectory.mockReset();
  mockSaveAs.mockReset();
  mockOpenInNewWindow.mockReset();
  mockCloneToEphemeral.mockReset();
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
    expect(mockSaveAs).not.toHaveBeenCalled();
  });

  it("opens a save dialog for untitled notebooks", async () => {
    mockSaveDialog.mockResolvedValueOnce(
      "/home/user/notebooks/MyNotebook.ipynb",
    );

    const result = await saveNotebook(stubHost, flushSync, false);

    expect(result).toBe(true);
    expect(mockGetDefaultSaveDirectory).toHaveBeenCalledTimes(1);
    expect(mockSaveDialog).toHaveBeenCalledTimes(1);
    expect(mockSaveAs).toHaveBeenCalledWith("/home/user/notebooks/MyNotebook.ipynb");
    expect(mockSendRequest).not.toHaveBeenCalled();
  });

  it("returns false when the save dialog is cancelled", async () => {
    mockGetDefaultSaveDirectory.mockResolvedValueOnce("/tmp");
    mockSaveDialog.mockResolvedValueOnce(null);

    const result = await saveNotebook(stubHost, flushSync, false);

    expect(result).toBe(false);
    expect(mockSaveAs).not.toHaveBeenCalled();
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

    await openNotebookFile(stubHost);

    expect(mockOpenDialog).toHaveBeenCalledTimes(1);
    expect(mockOpenInNewWindow).toHaveBeenCalledWith("/path/to/notebook.ipynb");
  });

  it("does nothing when the dialog is cancelled", async () => {
    mockOpenDialog.mockResolvedValueOnce(null);

    await openNotebookFile(stubHost);

    expect(mockOpenInNewWindow).not.toHaveBeenCalled();
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
    await cloneNotebookFile(stubHost);

    expect(mockCloneToEphemeral).toHaveBeenCalledTimes(1);

    // No dialog, no save-directory lookup, no legacy path construction.
    expect(mockSaveDialog).not.toHaveBeenCalled();
    expect(mockGetDefaultSaveDirectory).not.toHaveBeenCalled();
    expect(mockOpenInNewWindow).not.toHaveBeenCalled();
  });

  it("does not throw on error", async () => {
    mockCloneToEphemeral.mockRejectedValue(new Error("clone failed"));

    await expect(cloneNotebookFile(stubHost)).resolves.toBeUndefined();
  });
});
