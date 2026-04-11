import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { JupyterOutput } from "../../types";
import {
  type CellSnapshot,
  cellSnapshotsToNotebookCells,
  cellSnapshotsToNotebookCellsSync,
  outputCacheKey,
  resolveOutput,
  reuseOutputsIfUnchanged,
} from "../materialize-cells";

// ---------------------------------------------------------------------------
// Mock fetch globally for blob-store resolution tests
// ---------------------------------------------------------------------------

const mockFetch =
  vi.fn<(input: RequestInfo | URL, init?: RequestInit) => Promise<Response>>();

beforeEach(() => {
  vi.stubGlobal("fetch", mockFetch);
});

afterEach(() => {
  mockFetch.mockReset();
  vi.unstubAllGlobals();
});

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function streamOutput(name: "stdout" | "stderr", text: string): JupyterOutput {
  return { output_type: "stream", name, text };
}

function codeSnapshot(
  id: string,
  source: string,
  outputs: unknown[] = [],
  executionCount = "null",
): CellSnapshot {
  return {
    id,
    cell_type: "code",
    position: "80",
    source,
    execution_count: executionCount,
    outputs,
    metadata: {},
  };
}

function markdownSnapshot(
  id: string,
  source: string,
  resolvedAssets?: Record<string, string>,
): CellSnapshot {
  return {
    id,
    cell_type: "markdown",
    position: "80",
    source,
    execution_count: "null",
    outputs: [],
    metadata: {},
    resolved_assets: resolvedAssets,
  };
}

function rawSnapshot(id: string, source: string): CellSnapshot {
  return {
    id,
    cell_type: "raw",
    position: "80",
    source,
    execution_count: "null",
    outputs: [],
    metadata: {},
  };
}

// ---------------------------------------------------------------------------
// reuseOutputsIfUnchanged
// ---------------------------------------------------------------------------

describe("reuseOutputsIfUnchanged", () => {
  it("returns previous array when all elements are referentially identical", () => {
    const a: JupyterOutput = {
      output_type: "stream",
      name: "stdout",
      text: "hello",
    };
    const b: JupyterOutput = {
      output_type: "stream",
      name: "stderr",
      text: "err",
    };
    const previous = [a, b];
    const resolved = [a, b]; // same references, different array

    const result = reuseOutputsIfUnchanged(resolved, previous);
    expect(result).toBe(previous); // same array reference
  });

  it("returns resolved array when an element differs", () => {
    const a: JupyterOutput = {
      output_type: "stream",
      name: "stdout",
      text: "hello",
    };
    const b: JupyterOutput = {
      output_type: "stream",
      name: "stdout",
      text: "hello",
    };
    const previous = [a];
    const resolved = [b]; // same content, different object

    const result = reuseOutputsIfUnchanged(resolved, previous);
    expect(result).toBe(resolved);
    expect(result).not.toBe(previous);
  });

  it("returns resolved array when lengths differ", () => {
    const a: JupyterOutput = {
      output_type: "stream",
      name: "stdout",
      text: "hello",
    };
    const previous = [a];
    const resolved = [a, a];

    const result = reuseOutputsIfUnchanged(resolved, previous);
    expect(result).toBe(resolved);
  });

  it("returns resolved array when previous is undefined", () => {
    const a: JupyterOutput = {
      output_type: "stream",
      name: "stdout",
      text: "hello",
    };
    const resolved = [a];

    const result = reuseOutputsIfUnchanged(resolved, undefined);
    expect(result).toBe(resolved);
  });

  it("returns previous for empty arrays", () => {
    const previous: JupyterOutput[] = [];
    const resolved: JupyterOutput[] = [];

    const result = reuseOutputsIfUnchanged(resolved, previous);
    expect(result).toBe(previous);
  });
});

// ---------------------------------------------------------------------------
// resolveOutput
// ---------------------------------------------------------------------------

describe("resolveOutput", () => {
  it("returns cached value on cache hit", async () => {
    const cached: JupyterOutput = streamOutput("stdout", "cached");
    const cache = new Map<string, JupyterOutput>();
    const manifest = {
      output_type: "stream",
      name: "stdout",
      text: { inline: "cached" },
    };
    cache.set(outputCacheKey(manifest), cached);

    const result = await resolveOutput(manifest, null, cache);
    expect(result).toBe(cached);
    expect(mockFetch).not.toHaveBeenCalled();
  });

  it("resolves structured manifest object with inline refs", async () => {
    const manifest = {
      output_type: "stream",
      name: "stdout",
      text: { inline: "hello\n" },
    };
    const cache = new Map<string, JupyterOutput>();

    const result = await resolveOutput(manifest, 8765, cache);
    expect(result).toEqual({
      output_type: "stream",
      name: "stdout",
      text: "hello\n",
    });
  });

  it("resolves display_data manifest with inline data", async () => {
    const manifest = {
      output_type: "display_data",
      data: {
        "text/plain": { inline: "hi" },
        "text/html": { inline: "<b>hi</b>" },
      },
      metadata: { isolated: true },
    };
    const cache = new Map<string, JupyterOutput>();

    const result = await resolveOutput(manifest, 8765, cache);
    expect(result).toEqual({
      output_type: "display_data",
      data: { "text/plain": "hi", "text/html": "<b>hi</b>" },
      metadata: { isolated: true },
      display_id: undefined,
    });
  });

  it("resolves manifest with blob ref by fetching from blob server", async () => {
    const manifest = {
      output_type: "stream",
      name: "stdout",
      text: { blob: "abc123hash", size: 5000 },
    };
    const cache = new Map<string, JupyterOutput>();
    const blobPort = 8765;

    mockFetch.mockResolvedValueOnce(
      new Response("big text output", { status: 200 }),
    );

    const result = await resolveOutput(manifest, blobPort, cache);
    expect(result).toEqual({
      output_type: "stream",
      name: "stdout",
      text: "big text output",
    });
    expect(mockFetch).toHaveBeenCalledWith(
      `http://127.0.0.1:${blobPort}/blob/abc123hash`,
    );
  });

  it("caches resolved manifest output", async () => {
    const manifest = {
      output_type: "stream",
      name: "stdout",
      text: { inline: "cached manifest" },
    };
    const cache = new Map<string, JupyterOutput>();
    const key = outputCacheKey(manifest);

    await resolveOutput(manifest, 8765, cache);
    expect(cache.has(key)).toBe(true);

    // Second call should hit cache without resolving
    const result = await resolveOutput(manifest, 8765, cache);
    expect(result).toEqual({
      output_type: "stream",
      name: "stdout",
      text: "cached manifest",
    });
  });

  it("returns null for manifest object when blobPort is null", async () => {
    const manifest = {
      output_type: "stream",
      name: "stdout",
      text: { blob: "abc123", size: 100 },
    };
    const cache = new Map<string, JupyterOutput>();

    const result = await resolveOutput(manifest, null, cache);
    expect(result).toBeNull();
  });

  it("returns raw JupyterOutput object without ContentRefs", async () => {
    const output = {
      output_type: "stream",
      name: "stdout",
      text: "already resolved",
    };
    const cache = new Map<string, JupyterOutput>();

    const result = await resolveOutput(output, null, cache);
    expect(result).toEqual(output);
  });

  it("parses legacy JSON output string (backward compat)", async () => {
    const outputJson = JSON.stringify({
      output_type: "stream",
      name: "stdout",
      text: "hello\n",
    });
    const cache = new Map<string, JupyterOutput>();

    const result = await resolveOutput(outputJson, null, cache);
    expect(result).toEqual({
      output_type: "stream",
      name: "stdout",
      text: "hello\n",
    });
  });

  it("caches parsed legacy JSON output", async () => {
    const outputJson = JSON.stringify({
      output_type: "execute_result",
      data: { "text/plain": "42" },
      metadata: {},
      execution_count: 1,
    });
    const cache = new Map<string, JupyterOutput>();

    await resolveOutput(outputJson, null, cache);
    expect(cache.has(outputJson)).toBe(true);

    // Second call should hit cache
    const result = await resolveOutput(outputJson, null, cache);
    expect(result).toEqual(cache.get(outputJson));
  });

  it("returns null for invalid JSON string", async () => {
    const cache = new Map<string, JupyterOutput>();
    const result = await resolveOutput("not valid json{{{", null, cache);
    expect(result).toBeNull();
  });

  it("returns null for non-parseable string (e.g. hex hash)", async () => {
    const hash = "a".repeat(64);
    const cache = new Map<string, JupyterOutput>();

    const result = await resolveOutput(hash, null, cache);
    expect(result).toBeNull();
  });

  it("returns null for unrecognized types", async () => {
    const cache = new Map<string, JupyterOutput>();
    const result = await resolveOutput(42, null, cache);
    expect(result).toBeNull();
  });

  it("handles execute_result object correctly", async () => {
    const output = {
      output_type: "execute_result",
      data: { "text/plain": "2", "text/html": "<b>2</b>" },
      metadata: {},
      execution_count: 5,
    };
    const cache = new Map<string, JupyterOutput>();

    const result = await resolveOutput(output, null, cache);
    expect(result).toEqual(output);
  });

  it("handles error output object correctly", async () => {
    const output = {
      output_type: "error",
      ename: "ValueError",
      evalue: "bad value",
      traceback: [
        "\u001b[0;31m---------------------------------------------------------------------------\u001b[0m",
        "\u001b[0;31mValueError\u001b[0m: bad value",
      ],
    };
    const cache = new Map<string, JupyterOutput>();

    const result = await resolveOutput(output, null, cache);
    expect(result).toEqual(output);
  });
});

// ---------------------------------------------------------------------------
// cellSnapshotsToNotebookCells
// ---------------------------------------------------------------------------

describe("cellSnapshotsToNotebookCells", () => {
  it("returns empty array for empty snapshots", async () => {
    const cells = await cellSnapshotsToNotebookCells([], null, new Map());
    expect(cells).toEqual([]);
  });

  it("converts a code cell with structured manifest outputs", async () => {
    const manifest = {
      output_type: "stream",
      name: "stdout",
      text: { inline: "hello\n" },
    };
    const snap = codeSnapshot("c1", "print('hello')", [manifest], "1");

    const cells = await cellSnapshotsToNotebookCells([snap], 8765, new Map());
    expect(cells).toHaveLength(1);
    expect(cells[0]).toEqual({
      id: "c1",
      cell_type: "code",
      source: "print('hello')",
      execution_count: 1,
      outputs: [{ output_type: "stream", name: "stdout", text: "hello\n" }],
      metadata: {},
    });
  });

  it("converts a code cell with raw JupyterOutput objects (no ContentRefs)", async () => {
    const output = { output_type: "stream", name: "stdout", text: "hello\n" };
    const snap = codeSnapshot("c1", "print('hello')", [output], "1");

    const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());
    expect(cells).toHaveLength(1);
    expect(cells[0]).toEqual({
      id: "c1",
      cell_type: "code",
      source: "print('hello')",
      execution_count: 1,
      outputs: [{ output_type: "stream", name: "stdout", text: "hello\n" }],
      metadata: {},
    });
  });

  it("converts a code cell with legacy JSON string outputs", async () => {
    const output = { output_type: "stream", name: "stdout", text: "hello\n" };
    const snap = codeSnapshot(
      "c1",
      "print('hello')",
      [JSON.stringify(output)],
      "1",
    );

    const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());
    expect(cells).toHaveLength(1);
    expect(cells[0]).toEqual({
      id: "c1",
      cell_type: "code",
      source: "print('hello')",
      execution_count: 1,
      outputs: [{ output_type: "stream", name: "stdout", text: "hello\n" }],
      metadata: {},
    });
  });

  it("converts a code cell with no outputs", async () => {
    const snap = codeSnapshot("c1", "x = 1", [], "3");

    const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());
    expect(cells).toHaveLength(1);
    expect(cells[0]).toEqual({
      id: "c1",
      cell_type: "code",
      source: "x = 1",
      execution_count: 3,
      outputs: [],
      metadata: {},
    });
  });

  it("converts a markdown cell", async () => {
    const snap = markdownSnapshot("m1", "# Title");

    const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());
    expect(cells).toHaveLength(1);
    expect(cells[0]).toEqual({
      id: "m1",
      cell_type: "markdown",
      source: "# Title",
      metadata: {},
    });
  });

  it("preserves resolved markdown assets", async () => {
    const snap = markdownSnapshot("m1", "![x](attachment:image.png)", {
      "attachment:image.png": "abc123",
    });

    const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());
    expect(cells[0]).toEqual({
      id: "m1",
      cell_type: "markdown",
      source: "![x](attachment:image.png)",
      metadata: {},
      resolvedAssets: { "attachment:image.png": "abc123" },
    });
  });

  it("preserves resolved markdown assets during sync materialization", () => {
    const snap = markdownSnapshot("m1", "![x](images/foo.png)", {
      "images/foo.png": "abc123",
    });

    const cells = cellSnapshotsToNotebookCellsSync([snap], new Map());
    expect(cells[0]).toEqual({
      id: "m1",
      cell_type: "markdown",
      source: "![x](images/foo.png)",
      metadata: {},
      resolvedAssets: { "images/foo.png": "abc123" },
    });
  });

  it("converts a raw cell", async () => {
    const snap = rawSnapshot("r1", "raw content");

    const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());
    expect(cells).toHaveLength(1);
    expect(cells[0]).toEqual({
      id: "r1",
      cell_type: "raw",
      source: "raw content",
      metadata: {},
    });
  });

  describe("execution_count parsing", () => {
    it('parses "null" as null', async () => {
      const snap = codeSnapshot("c1", "", [], "null");
      const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());
      if (cells[0].cell_type === "code") {
        expect(cells[0].execution_count).toBeNull();
      }
    });

    it('parses "5" as 5', async () => {
      const snap = codeSnapshot("c1", "", [], "5");
      const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());
      if (cells[0].cell_type === "code") {
        expect(cells[0].execution_count).toBe(5);
      }
    });

    it('parses "0" as 0', async () => {
      const snap = codeSnapshot("c1", "", [], "0");
      const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());
      if (cells[0].cell_type === "code") {
        expect(cells[0].execution_count).toBe(0);
      }
    });

    it('parses "100" as 100', async () => {
      const snap = codeSnapshot("c1", "", [], "100");
      const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());
      if (cells[0].cell_type === "code") {
        expect(cells[0].execution_count).toBe(100);
      }
    });

    it("parses non-numeric string as null (NaN fallback)", async () => {
      const snap = codeSnapshot("c1", "", [], "not_a_number");
      const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());
      if (cells[0].cell_type === "code") {
        expect(cells[0].execution_count).toBeNull();
      }
    });

    it("parses empty string as null (NaN fallback)", async () => {
      const snap = codeSnapshot("c1", "", [], "");
      const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());
      if (cells[0].cell_type === "code") {
        expect(cells[0].execution_count).toBeNull();
      }
    });
  });

  it("filters out null (unparseable) outputs", async () => {
    const validManifest = {
      output_type: "stream",
      name: "stdout",
      text: { inline: "ok\n" },
    };
    const snap = codeSnapshot(
      "c1",
      "",
      [validManifest, "invalid json{{{"],
      "1",
    );

    const cells = await cellSnapshotsToNotebookCells([snap], 8765, new Map());
    if (cells[0].cell_type === "code") {
      expect(cells[0].outputs).toHaveLength(1);
      expect(cells[0].outputs[0]).toEqual({
        output_type: "stream",
        name: "stdout",
        text: "ok\n",
      });
    }
  });

  it("passes through consecutive streams without merging (daemon consolidates)", async () => {
    const out1 = {
      output_type: "stream",
      name: "stdout",
      text: { inline: "line1\n" },
    };
    const out2 = {
      output_type: "stream",
      name: "stdout",
      text: { inline: "line2\n" },
    };
    const snap = codeSnapshot("c1", "print(...)", [out1, out2], "1");

    // The daemon's StreamTerminals consolidates streams via terminal
    // emulation before writing to the Automerge doc. The frontend no
    // longer merges — it passes outputs through as-is.
    const cells = await cellSnapshotsToNotebookCells([snap], 8765, new Map());
    if (cells[0].cell_type === "code") {
      expect(cells[0].outputs).toHaveLength(2);
      expect(cells[0].outputs[0]).toEqual({
        output_type: "stream",
        name: "stdout",
        text: "line1\n",
      });
      expect(cells[0].outputs[1]).toEqual({
        output_type: "stream",
        name: "stdout",
        text: "line2\n",
      });
    }
  });

  it("does not merge streams with different names", async () => {
    const stdout = {
      output_type: "stream",
      name: "stdout",
      text: { inline: "out\n" },
    };
    const stderr = {
      output_type: "stream",
      name: "stderr",
      text: { inline: "err\n" },
    };
    const snap = codeSnapshot("c1", "", [stdout, stderr], "1");

    const cells = await cellSnapshotsToNotebookCells([snap], 8765, new Map());
    if (cells[0].cell_type === "code") {
      expect(cells[0].outputs).toHaveLength(2);
    }
  });

  it("converts mixed cell types in order", async () => {
    const streamManifest = {
      output_type: "stream",
      name: "stdout",
      text: { inline: "42\n" },
    };
    const snaps: CellSnapshot[] = [
      markdownSnapshot("m1", "# Intro"),
      codeSnapshot("c1", "print(42)", [streamManifest], "1"),
      rawSnapshot("r1", "---"),
      codeSnapshot("c2", "x", [], "null"),
      markdownSnapshot("m2", "## End"),
    ];

    const cells = await cellSnapshotsToNotebookCells(snaps, 8765, new Map());
    expect(cells).toHaveLength(5);
    expect(cells.map((c) => c.cell_type)).toEqual([
      "markdown",
      "code",
      "raw",
      "code",
      "markdown",
    ]);
    expect(cells.map((c) => c.id)).toEqual(["m1", "c1", "r1", "c2", "m2"]);
  });

  it("preserves cell source verbatim", async () => {
    const source =
      "  def foo():\n    return 'bar'\n\n# comment with special chars: <>&\"'";
    const snap = codeSnapshot("c1", source, [], "null");

    const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());
    expect(cells[0].source).toBe(source);
  });

  it("uses shared cache across all output resolutions", async () => {
    const manifest = {
      output_type: "execute_result",
      data: { "text/plain": { inline: "same" } },
      execution_count: 1,
    };
    // Two cells reference the same manifest object shape
    const snaps: CellSnapshot[] = [
      codeSnapshot("c1", "", [manifest], "1"),
      codeSnapshot("c2", "", [manifest], "2"),
    ];
    const cache = new Map<string, JupyterOutput>();

    const cells = await cellSnapshotsToNotebookCells(snaps, 8765, cache);
    expect(cache.size).toBe(1);
    if (cells[0].cell_type === "code" && cells[1].cell_type === "code") {
      expect(cells[0].outputs[0]).toEqual(cells[1].outputs[0]);
    }
  });

  it("handles code cell with multiple output types", async () => {
    const outputs = [
      {
        output_type: "stream",
        name: "stdout",
        text: { inline: "computing...\n" },
      },
      {
        output_type: "execute_result",
        data: {
          "text/plain": { inline: "42" },
          "text/html": { inline: "<b>42</b>" },
        },
        execution_count: 3,
      },
      {
        output_type: "display_data",
        data: { "image/png": { url: "http://127.0.0.1:8765/blob/imgblob" } },
      },
    ];
    const snap = codeSnapshot("c1", "compute()", outputs, "3");

    const cells = await cellSnapshotsToNotebookCells([snap], 8765, new Map());
    if (cells[0].cell_type === "code") {
      expect(cells[0].outputs).toHaveLength(3);
      expect(cells[0].outputs[0].output_type).toBe("stream");
      expect(cells[0].outputs[1].output_type).toBe("execute_result");
      expect(cells[0].outputs[2].output_type).toBe("display_data");
    }
  });

  it("resolves manifest object outputs with blob refs when blobPort is provided", async () => {
    const manifest = {
      output_type: "stream",
      name: "stdout",
      text: { blob: "blobhash123", size: 5000 },
    };
    mockFetch.mockResolvedValueOnce(
      new Response("from manifest\n", { status: 200 }),
    );

    const snap = codeSnapshot("c1", "", [manifest], "1");
    const cells = await cellSnapshotsToNotebookCells([snap], 9999, new Map());

    if (cells[0].cell_type === "code") {
      expect(cells[0].outputs).toHaveLength(1);
      expect(cells[0].outputs[0]).toEqual({
        output_type: "stream",
        name: "stdout",
        text: "from manifest\n",
      });
    }
    expect(mockFetch).toHaveBeenCalledWith(
      "http://127.0.0.1:9999/blob/blobhash123",
    );
  });

  it("handles manifest object with no blobPort gracefully", async () => {
    const manifest = {
      output_type: "stream",
      name: "stdout",
      text: { blob: "blobhash", size: 100 },
    };
    const snap = codeSnapshot("c1", "", [manifest], "1");

    const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());
    if (cells[0].cell_type === "code") {
      // Manifest with blob ref and no blobPort resolves to null and is filtered
      expect(cells[0].outputs).toHaveLength(0);
    }
  });

  it("markdown cells do not include outputs or execution_count", async () => {
    const snap = markdownSnapshot("m1", "text");
    const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());

    expect(cells[0]).toEqual({
      id: "m1",
      cell_type: "markdown",
      source: "text",
      metadata: {},
    });
    expect(cells[0]).not.toHaveProperty("outputs");
    expect(cells[0]).not.toHaveProperty("execution_count");
  });

  it("raw cells do not include outputs or execution_count", async () => {
    const snap = rawSnapshot("r1", "content");
    const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());

    expect(cells[0]).toEqual({
      id: "r1",
      cell_type: "raw",
      source: "content",
      metadata: {},
    });
    expect(cells[0]).not.toHaveProperty("outputs");
    expect(cells[0]).not.toHaveProperty("execution_count");
  });

  it("handles error output manifest in code cells", async () => {
    const traceback = [
      "\u001b[0;31mZeroDivisionError\u001b[0m: division by zero",
    ];
    const errManifest = {
      output_type: "error",
      ename: "ZeroDivisionError",
      evalue: "division by zero",
      traceback: { inline: JSON.stringify(traceback) },
    };
    const snap = codeSnapshot("c1", "1/0", [errManifest], "1");

    const cells = await cellSnapshotsToNotebookCells([snap], 8765, new Map());
    if (cells[0].cell_type === "code") {
      expect(cells[0].outputs).toHaveLength(1);
      const out = cells[0].outputs[0];
      expect(out.output_type).toBe("error");
      if (out.output_type === "error") {
        expect(out.ename).toBe("ZeroDivisionError");
        expect(out.evalue).toBe("division by zero");
        expect(out.traceback).toHaveLength(1);
      }
    }
  });

  it("handles raw error output object (no ContentRefs) in code cells", async () => {
    const errOutput = {
      output_type: "error",
      ename: "ValueError",
      evalue: "bad",
      traceback: ["ValueError: bad"],
    };
    const snap = codeSnapshot("c1", "1/0", [errOutput], "1");

    const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());
    if (cells[0].cell_type === "code") {
      expect(cells[0].outputs).toHaveLength(1);
      expect(cells[0].outputs[0]).toEqual(errOutput);
    }
  });

  it("handles large number of cells", async () => {
    const snaps: CellSnapshot[] = [];
    for (let i = 0; i < 100; i++) {
      snaps.push(codeSnapshot(`c${i}`, `x = ${i}`, [], String(i)));
    }

    const cells = await cellSnapshotsToNotebookCells(snaps, null, new Map());
    expect(cells).toHaveLength(100);
    for (let i = 0; i < 100; i++) {
      expect(cells[i].id).toBe(`c${i}`);
      if (cells[i].cell_type === "code") {
        expect(cells[i].cell_type).toBe("code");
        expect((cells[i] as { execution_count: number }).execution_count).toBe(
          i,
        );
      }
    }
  });
});
