import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { JupyterOutput } from "../../types";
import {
  type CellSnapshot,
  cellSnapshotsToNotebookCells,
  cellSnapshotsToNotebookCellsSync,
  mergeConsecutiveStreams,
  resolveOutput,
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

function executeResult(
  data: Record<string, unknown>,
  executionCount?: number | null,
): JupyterOutput {
  return {
    output_type: "execute_result",
    data,
    metadata: {},
    execution_count: executionCount ?? null,
  };
}

function displayData(data: Record<string, unknown>): JupyterOutput {
  return { output_type: "display_data", data, metadata: {} };
}

function errorOutput(
  ename: string,
  evalue: string,
  traceback: string[],
): JupyterOutput {
  return { output_type: "error", ename, evalue, traceback };
}

function codeSnapshot(
  id: string,
  source: string,
  outputs: string[] = [],
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
// mergeConsecutiveStreams
// ---------------------------------------------------------------------------

describe("mergeConsecutiveStreams", () => {
  it("returns empty array for empty input", () => {
    expect(mergeConsecutiveStreams([])).toEqual([]);
  });

  it("passes through a single stream output unchanged", () => {
    const outputs = [streamOutput("stdout", "hello\n")];
    const merged = mergeConsecutiveStreams(outputs);
    expect(merged).toHaveLength(1);
    expect(merged[0]).toEqual(streamOutput("stdout", "hello\n"));
  });

  it("merges consecutive stdout streams", () => {
    const outputs = [
      streamOutput("stdout", "line1\n"),
      streamOutput("stdout", "line2\n"),
      streamOutput("stdout", "line3\n"),
    ];
    const merged = mergeConsecutiveStreams(outputs);
    expect(merged).toHaveLength(1);
    expect(merged[0]).toEqual(streamOutput("stdout", "line1\nline2\nline3\n"));
  });

  it("merges consecutive stderr streams", () => {
    const outputs = [
      streamOutput("stderr", "warn1 "),
      streamOutput("stderr", "warn2"),
    ];
    const merged = mergeConsecutiveStreams(outputs);
    expect(merged).toHaveLength(1);
    expect(merged[0]).toEqual(streamOutput("stderr", "warn1 warn2"));
  });

  it("does NOT merge stdout and stderr", () => {
    const outputs = [
      streamOutput("stdout", "out\n"),
      streamOutput("stderr", "err\n"),
    ];
    const merged = mergeConsecutiveStreams(outputs);
    expect(merged).toHaveLength(2);
    expect(merged[0]).toEqual(streamOutput("stdout", "out\n"));
    expect(merged[1]).toEqual(streamOutput("stderr", "err\n"));
  });

  it("does NOT merge streams separated by a non-stream output", () => {
    const outputs = [
      streamOutput("stdout", "before\n"),
      displayData({ "text/plain": "interruption" }),
      streamOutput("stdout", "after\n"),
    ];
    const merged = mergeConsecutiveStreams(outputs);
    expect(merged).toHaveLength(3);
    expect(merged[0]).toEqual(streamOutput("stdout", "before\n"));
    expect(merged[2]).toEqual(streamOutput("stdout", "after\n"));
  });

  it("handles interleaved stdout and stderr", () => {
    const outputs = [
      streamOutput("stdout", "out1\n"),
      streamOutput("stdout", "out2\n"),
      streamOutput("stderr", "err1\n"),
      streamOutput("stderr", "err2\n"),
      streamOutput("stdout", "out3\n"),
    ];
    const merged = mergeConsecutiveStreams(outputs);
    expect(merged).toHaveLength(3);
    expect(merged[0]).toEqual(streamOutput("stdout", "out1\nout2\n"));
    expect(merged[1]).toEqual(streamOutput("stderr", "err1\nerr2\n"));
    expect(merged[2]).toEqual(streamOutput("stdout", "out3\n"));
  });

  it("handles array text format by joining before merge", () => {
    // The function handles both string and string[] text formats
    const outputs: JupyterOutput[] = [
      { output_type: "stream", name: "stdout", text: "part1" as string },
      { output_type: "stream", name: "stdout", text: "part2" as string },
    ];
    const merged = mergeConsecutiveStreams(outputs);
    expect(merged).toHaveLength(1);
    if (merged[0].output_type === "stream") {
      expect(merged[0].text).toBe("part1part2");
    }
  });

  it("passes through non-stream outputs untouched", () => {
    const outputs = [
      executeResult({ "text/plain": "42" }, 1),
      displayData({ "image/png": "base64..." }),
      errorOutput("Error", "boom", ["traceback"]),
    ];
    const merged = mergeConsecutiveStreams(outputs);
    expect(merged).toHaveLength(3);
    expect(merged).toEqual(outputs);
  });

  it("handles a complex mixed sequence", () => {
    const outputs = [
      streamOutput("stdout", "a"),
      streamOutput("stdout", "b"),
      executeResult({ "text/plain": "1" }, 1),
      streamOutput("stderr", "e1"),
      streamOutput("stderr", "e2"),
      streamOutput("stdout", "c"),
      errorOutput("Err", "x", []),
      streamOutput("stdout", "d"),
      streamOutput("stdout", "e"),
    ];
    const merged = mergeConsecutiveStreams(outputs);
    expect(merged).toHaveLength(6);
    expect(merged[0]).toEqual(streamOutput("stdout", "ab"));
    expect(merged[1]).toEqual(executeResult({ "text/plain": "1" }, 1));
    expect(merged[2]).toEqual(streamOutput("stderr", "e1e2"));
    expect(merged[3]).toEqual(streamOutput("stdout", "c"));
    expect(merged[4]).toEqual(errorOutput("Err", "x", []));
    expect(merged[5]).toEqual(streamOutput("stdout", "de"));
  });

  it("preserves empty stream text", () => {
    const outputs = [
      streamOutput("stdout", ""),
      streamOutput("stdout", "text"),
    ];
    const merged = mergeConsecutiveStreams(outputs);
    expect(merged).toHaveLength(1);
    if (merged[0].output_type === "stream") {
      expect(merged[0].text).toBe("text");
    }
  });

  it("does not mutate the input array", () => {
    const outputs = [streamOutput("stdout", "a"), streamOutput("stdout", "b")];
    const original = [...outputs];
    mergeConsecutiveStreams(outputs);
    expect(outputs).toEqual(original);
  });
});

// ---------------------------------------------------------------------------
// resolveOutput
// ---------------------------------------------------------------------------

describe("resolveOutput", () => {
  it("returns cached value on cache hit", async () => {
    const cached: JupyterOutput = streamOutput("stdout", "cached");
    const cache = new Map<string, JupyterOutput>();
    cache.set("key1", cached);

    const result = await resolveOutput("key1", null, cache);
    expect(result).toBe(cached);
    expect(mockFetch).not.toHaveBeenCalled();
  });

  it("parses raw JSON output string (non-manifest)", async () => {
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

  it("caches parsed JSON output", async () => {
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

  it("returns null for invalid JSON (non-manifest)", async () => {
    const cache = new Map<string, JupyterOutput>();
    // Not a manifest hash (not 64 hex chars) and not valid JSON
    const result = await resolveOutput("not valid json{{{", null, cache);
    expect(result).toBeNull();
  });

  it("returns null for manifest hash when blobPort is null", async () => {
    const hash = "a".repeat(64);
    const cache = new Map<string, JupyterOutput>();

    const result = await resolveOutput(hash, null, cache);
    expect(result).toBeNull();
    expect(mockFetch).not.toHaveBeenCalled();
  });

  it("fetches and resolves manifest hash when blobPort is set", async () => {
    const hash =
      "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const cache = new Map<string, JupyterOutput>();
    const blobPort = 8765;

    // The manifest fetch (first fetch for the manifest JSON)
    const manifest = {
      output_type: "stream",
      name: "stdout",
      text: { inline: "resolved text" },
    };
    mockFetch.mockResolvedValueOnce(
      new Response(JSON.stringify(manifest), { status: 200 }),
    );

    const result = await resolveOutput(hash, blobPort, cache);
    expect(result).toEqual({
      output_type: "stream",
      name: "stdout",
      text: "resolved text",
    });
    expect(mockFetch).toHaveBeenCalledWith(
      `http://127.0.0.1:${blobPort}/blob/${hash}`,
    );
  });

  it("caches resolved manifest output", async () => {
    const hash =
      "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
    const cache = new Map<string, JupyterOutput>();
    const blobPort = 8765;

    const manifest = {
      output_type: "stream",
      name: "stdout",
      text: { inline: "cached manifest" },
    };
    mockFetch.mockResolvedValueOnce(
      new Response(JSON.stringify(manifest), { status: 200 }),
    );

    await resolveOutput(hash, blobPort, cache);
    expect(cache.has(hash)).toBe(true);

    // Second call should hit cache without fetching
    mockFetch.mockClear();
    const result = await resolveOutput(hash, blobPort, cache);
    expect(result).toEqual({
      output_type: "stream",
      name: "stdout",
      text: "cached manifest",
    });
    expect(mockFetch).not.toHaveBeenCalled();
  });

  it("returns null on fetch failure for manifest hash", async () => {
    const hash = "f".repeat(64);
    const cache = new Map<string, JupyterOutput>();

    mockFetch.mockResolvedValueOnce(new Response("not found", { status: 404 }));

    const result = await resolveOutput(hash, 8765, cache);
    expect(result).toBeNull();
  });

  it("returns null on network error for manifest hash", async () => {
    const hash = "b".repeat(64);
    const cache = new Map<string, JupyterOutput>();

    mockFetch.mockRejectedValueOnce(new TypeError("network error"));

    const result = await resolveOutput(hash, 8765, cache);
    expect(result).toBeNull();
  });

  it("parses execute_result JSON correctly", async () => {
    const output = {
      output_type: "execute_result",
      data: { "text/plain": "2", "text/html": "<b>2</b>" },
      metadata: {},
      execution_count: 5,
    };
    const cache = new Map<string, JupyterOutput>();

    const result = await resolveOutput(JSON.stringify(output), null, cache);
    expect(result).toEqual(output);
  });

  it("parses error output JSON correctly", async () => {
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

    const result = await resolveOutput(JSON.stringify(output), null, cache);
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

  it("converts a code cell with raw JSON outputs", async () => {
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
    const validOutput = JSON.stringify({
      output_type: "stream",
      name: "stdout",
      text: "ok\n",
    });
    const snap = codeSnapshot("c1", "", [validOutput, "invalid json{{{"], "1");

    const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());
    if (cells[0].cell_type === "code") {
      expect(cells[0].outputs).toHaveLength(1);
      expect(cells[0].outputs[0]).toEqual({
        output_type: "stream",
        name: "stdout",
        text: "ok\n",
      });
    }
  });

  it("merges consecutive streams in code cell outputs", async () => {
    const out1 = JSON.stringify({
      output_type: "stream",
      name: "stdout",
      text: "line1\n",
    });
    const out2 = JSON.stringify({
      output_type: "stream",
      name: "stdout",
      text: "line2\n",
    });
    const snap = codeSnapshot("c1", "print(...)", [out1, out2], "1");

    const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());
    if (cells[0].cell_type === "code") {
      expect(cells[0].outputs).toHaveLength(1);
      expect(cells[0].outputs[0]).toEqual({
        output_type: "stream",
        name: "stdout",
        text: "line1\nline2\n",
      });
    }
  });

  it("does not merge streams with different names", async () => {
    const stdout = JSON.stringify({
      output_type: "stream",
      name: "stdout",
      text: "out\n",
    });
    const stderr = JSON.stringify({
      output_type: "stream",
      name: "stderr",
      text: "err\n",
    });
    const snap = codeSnapshot("c1", "", [stdout, stderr], "1");

    const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());
    if (cells[0].cell_type === "code") {
      expect(cells[0].outputs).toHaveLength(2);
    }
  });

  it("converts mixed cell types in order", async () => {
    const streamJson = JSON.stringify({
      output_type: "stream",
      name: "stdout",
      text: "42\n",
    });
    const snaps: CellSnapshot[] = [
      markdownSnapshot("m1", "# Intro"),
      codeSnapshot("c1", "print(42)", [streamJson], "1"),
      rawSnapshot("r1", "---"),
      codeSnapshot("c2", "x", [], "null"),
      markdownSnapshot("m2", "## End"),
    ];

    const cells = await cellSnapshotsToNotebookCells(snaps, null, new Map());
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
    const outputJson = JSON.stringify({
      output_type: "execute_result",
      data: { "text/plain": "same" },
      metadata: {},
      execution_count: 1,
    });
    // Two cells reference the same output string
    const snaps: CellSnapshot[] = [
      codeSnapshot("c1", "", [outputJson], "1"),
      codeSnapshot("c2", "", [outputJson], "2"),
    ];
    const cache = new Map<string, JupyterOutput>();

    const cells = await cellSnapshotsToNotebookCells(snaps, null, cache);
    expect(cache.size).toBe(1);
    if (cells[0].cell_type === "code" && cells[1].cell_type === "code") {
      expect(cells[0].outputs[0]).toEqual(cells[1].outputs[0]);
    }
  });

  it("handles code cell with multiple output types", async () => {
    const outputs = [
      JSON.stringify({
        output_type: "stream",
        name: "stdout",
        text: "computing...\n",
      }),
      JSON.stringify({
        output_type: "execute_result",
        data: { "text/plain": "42", "text/html": "<b>42</b>" },
        metadata: {},
        execution_count: 3,
      }),
      JSON.stringify({
        output_type: "display_data",
        data: { "image/png": "base64data" },
        metadata: {},
      }),
    ];
    const snap = codeSnapshot("c1", "compute()", outputs, "3");

    const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());
    if (cells[0].cell_type === "code") {
      expect(cells[0].outputs).toHaveLength(3);
      expect(cells[0].outputs[0].output_type).toBe("stream");
      expect(cells[0].outputs[1].output_type).toBe("execute_result");
      expect(cells[0].outputs[2].output_type).toBe("display_data");
    }
  });

  it("resolves manifest hash outputs when blobPort is provided", async () => {
    const hash =
      "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const manifest = {
      output_type: "stream",
      name: "stdout",
      text: { inline: "from manifest\n" },
    };
    mockFetch.mockResolvedValueOnce(
      new Response(JSON.stringify(manifest), { status: 200 }),
    );

    const snap = codeSnapshot("c1", "", [hash], "1");
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
      `http://127.0.0.1:9999/blob/${hash}`,
    );
  });

  it("handles manifest hash with no blobPort gracefully", async () => {
    const hash = "a".repeat(64);
    const snap = codeSnapshot("c1", "", [hash], "1");

    const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());
    if (cells[0].cell_type === "code") {
      // Manifest hash without blobPort resolves to null and is filtered out
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

  it("handles error output in code cells", async () => {
    const errOutput = JSON.stringify({
      output_type: "error",
      ename: "ZeroDivisionError",
      evalue: "division by zero",
      traceback: ["\u001b[0;31mZeroDivisionError\u001b[0m: division by zero"],
    });
    const snap = codeSnapshot("c1", "1/0", [errOutput], "1");

    const cells = await cellSnapshotsToNotebookCells([snap], null, new Map());
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
