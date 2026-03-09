import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  type ContentRef,
  isManifestHash,
  type OutputManifest,
  resolveContentRef,
  resolveDataBundle,
  resolveManifest,
} from "../manifest-resolution";

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
// isManifestHash
// ---------------------------------------------------------------------------

describe("isManifestHash", () => {
  it("returns true for a valid 64-char lowercase hex string", () => {
    const hash = "a".repeat(64);
    expect(isManifestHash(hash)).toBe(true);
  });

  it("returns true for a realistic SHA-256 hash", () => {
    expect(
      isManifestHash(
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
      ),
    ).toBe(true);
  });

  it("returns false for uppercase hex", () => {
    const hash = "A".repeat(64);
    expect(isManifestHash(hash)).toBe(false);
  });

  it("returns false for 63-char string", () => {
    expect(isManifestHash("a".repeat(63))).toBe(false);
  });

  it("returns false for 65-char string", () => {
    expect(isManifestHash("a".repeat(65))).toBe(false);
  });

  it("returns false for empty string", () => {
    expect(isManifestHash("")).toBe(false);
  });

  it("returns false for non-hex characters", () => {
    const hash = "g".repeat(64);
    expect(isManifestHash(hash)).toBe(false);
  });

  it("returns false for mixed valid/invalid chars at 64 length", () => {
    const hash = "a".repeat(63) + "z";
    expect(isManifestHash(hash)).toBe(false);
  });

  it("returns false for JSON strings", () => {
    expect(isManifestHash('{"output_type":"stream"}')).toBe(false);
  });

  it("returns false for strings with spaces", () => {
    const hash = "a".repeat(32) + " " + "b".repeat(31);
    expect(isManifestHash(hash)).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// resolveContentRef
// ---------------------------------------------------------------------------

describe("resolveContentRef", () => {
  const blobPort = 9876;

  it("returns inline content immediately without fetching", async () => {
    const ref: ContentRef = { inline: "hello world" };
    const result = await resolveContentRef(ref, blobPort);
    expect(result).toBe("hello world");
    expect(mockFetch).not.toHaveBeenCalled();
  });

  it("returns empty string for inline empty content", async () => {
    const ref: ContentRef = { inline: "" };
    const result = await resolveContentRef(ref, blobPort);
    expect(result).toBe("");
  });

  it("fetches blob content from the blob store", async () => {
    const blobHash = "abc123";
    const ref: ContentRef = { blob: blobHash, size: 42 };

    mockFetch.mockResolvedValueOnce(
      new Response("fetched content", { status: 200 }),
    );

    const result = await resolveContentRef(ref, blobPort);
    expect(result).toBe("fetched content");
    expect(mockFetch).toHaveBeenCalledWith(
      `http://127.0.0.1:${blobPort}/blob/${blobHash}`,
    );
  });

  it("throws on non-OK response from blob store", async () => {
    const ref: ContentRef = { blob: "deadbeef", size: 10 };

    mockFetch.mockResolvedValueOnce(new Response("not found", { status: 404 }));

    await expect(resolveContentRef(ref, blobPort)).rejects.toThrow(
      "Failed to fetch blob deadbeef: 404",
    );
  });

  it("uses the correct port in the URL", async () => {
    const ref: ContentRef = { blob: "hash123", size: 5 };
    mockFetch.mockResolvedValueOnce(new Response("ok", { status: 200 }));

    await resolveContentRef(ref, 5555);
    expect(mockFetch).toHaveBeenCalledWith(
      "http://127.0.0.1:5555/blob/hash123",
    );
  });
});

// ---------------------------------------------------------------------------
// resolveDataBundle
// ---------------------------------------------------------------------------

describe("resolveDataBundle", () => {
  const blobPort = 9876;

  it("resolves inline content refs to their values", async () => {
    const data: Record<string, ContentRef> = {
      "text/plain": { inline: "hello" },
      "text/html": { inline: "<b>hello</b>" },
    };

    const result = await resolveDataBundle(data, blobPort);
    expect(result).toEqual({
      "text/plain": "hello",
      "text/html": "<b>hello</b>",
    });
    expect(mockFetch).not.toHaveBeenCalled();
  });

  it("auto-parses JSON MIME types", async () => {
    const jsonObj = { key: "value", nested: { a: 1 } };
    const data: Record<string, ContentRef> = {
      "application/json": { inline: JSON.stringify(jsonObj) },
    };

    const result = await resolveDataBundle(data, blobPort);
    expect(result["application/json"]).toEqual(jsonObj);
  });

  it("auto-parses vnd+json MIME types", async () => {
    const vegaSpec = {
      $schema: "https://vega.github.io/schema/vega-lite/v5.json",
    };
    const data: Record<string, ContentRef> = {
      "application/vnd.vegalite.v5+json": { inline: JSON.stringify(vegaSpec) },
    };

    const result = await resolveDataBundle(data, blobPort);
    expect(result["application/vnd.vegalite.v5+json"]).toEqual(vegaSpec);
  });

  it("falls back to raw string for invalid JSON in json MIME type", async () => {
    const data: Record<string, ContentRef> = {
      "application/json": { inline: "not valid json{" },
    };

    const result = await resolveDataBundle(data, blobPort);
    expect(result["application/json"]).toBe("not valid json{");
  });

  it("does not parse non-JSON MIME types", async () => {
    const jsonString = '{"key":"value"}';
    const data: Record<string, ContentRef> = {
      "text/plain": { inline: jsonString },
    };

    const result = await resolveDataBundle(data, blobPort);
    expect(result["text/plain"]).toBe(jsonString);
  });

  it("resolves blob refs by fetching from the store", async () => {
    const data: Record<string, ContentRef> = {
      "image/png": { blob: "pnghash", size: 100 },
    };

    mockFetch.mockResolvedValueOnce(
      new Response("base64pngdata", { status: 200 }),
    );

    const result = await resolveDataBundle(data, blobPort);
    expect(result["image/png"]).toBe("base64pngdata");
  });

  it("handles mixed inline and blob refs", async () => {
    const data: Record<string, ContentRef> = {
      "text/plain": { inline: "fallback text" },
      "image/png": { blob: "pnghash", size: 200 },
    };

    mockFetch.mockResolvedValueOnce(
      new Response("pngblobcontent", { status: 200 }),
    );

    const result = await resolveDataBundle(data, blobPort);
    expect(result["text/plain"]).toBe("fallback text");
    expect(result["image/png"]).toBe("pngblobcontent");
  });

  it("handles empty data bundle", async () => {
    const result = await resolveDataBundle({}, blobPort);
    expect(result).toEqual({});
  });
});

// ---------------------------------------------------------------------------
// resolveManifest
// ---------------------------------------------------------------------------

describe("resolveManifest", () => {
  const blobPort = 9876;

  describe("display_data manifests", () => {
    it("resolves inline data refs", async () => {
      const manifest: OutputManifest = {
        output_type: "display_data",
        data: {
          "text/plain": { inline: "hello" },
          "text/html": { inline: "<b>hello</b>" },
        },
        metadata: { isolated: true },
        transient: { display_id: "d1" },
      };

      const output = await resolveManifest(manifest, blobPort);
      expect(output).toEqual({
        output_type: "display_data",
        data: {
          "text/plain": "hello",
          "text/html": "<b>hello</b>",
        },
        metadata: { isolated: true },
        display_id: "d1",
      });
    });

    it("defaults metadata to empty object when omitted", async () => {
      const manifest: OutputManifest = {
        output_type: "display_data",
        data: { "text/plain": { inline: "hi" } },
      };

      const output = await resolveManifest(manifest, blobPort);
      expect(output).toEqual({
        output_type: "display_data",
        data: { "text/plain": "hi" },
        metadata: {},
        display_id: undefined,
      });
    });

    it("resolves blob refs in data", async () => {
      const manifest: OutputManifest = {
        output_type: "display_data",
        data: {
          "image/png": { blob: "pnghash", size: 500 },
        },
      };

      mockFetch.mockResolvedValueOnce(
        new Response("base64png", { status: 200 }),
      );

      const output = await resolveManifest(manifest, blobPort);
      expect(output).toEqual({
        output_type: "display_data",
        data: { "image/png": "base64png" },
        metadata: {},
        display_id: undefined,
      });
    });
  });

  describe("execute_result manifests", () => {
    it("resolves with execution_count", async () => {
      const manifest: OutputManifest = {
        output_type: "execute_result",
        data: { "text/plain": { inline: "42" } },
        execution_count: 5,
      };

      const output = await resolveManifest(manifest, blobPort);
      expect(output).toEqual({
        output_type: "execute_result",
        data: { "text/plain": "42" },
        metadata: {},
        execution_count: 5,
        display_id: undefined,
      });
    });

    it("defaults execution_count to null when omitted", async () => {
      const manifest: OutputManifest = {
        output_type: "execute_result",
        data: { "text/plain": { inline: "result" } },
      };

      const output = await resolveManifest(manifest, blobPort);
      if (output.output_type === "execute_result") {
        expect(output.execution_count).toBeNull();
      }
    });

    it("preserves transient display_id", async () => {
      const manifest: OutputManifest = {
        output_type: "execute_result",
        data: { "text/plain": { inline: "x" } },
        transient: { display_id: "exec-d1" },
      };

      const output = await resolveManifest(manifest, blobPort);
      if (
        output.output_type === "execute_result" ||
        output.output_type === "display_data"
      ) {
        expect(output.display_id).toBe("exec-d1");
      }
    });

    it("auto-parses JSON MIME types in data", async () => {
      const manifest: OutputManifest = {
        output_type: "execute_result",
        data: {
          "application/json": { inline: '{"answer":42}' },
          "text/plain": { inline: "{'answer': 42}" },
        },
        execution_count: 1,
      };

      const output = await resolveManifest(manifest, blobPort);
      if (output.output_type === "execute_result") {
        expect(output.data["application/json"]).toEqual({ answer: 42 });
        expect(output.data["text/plain"]).toBe("{'answer': 42}");
      }
    });
  });

  describe("stream manifests", () => {
    it("resolves inline text", async () => {
      const manifest: OutputManifest = {
        output_type: "stream",
        name: "stdout",
        text: { inline: "hello world\n" },
      };

      const output = await resolveManifest(manifest, blobPort);
      expect(output).toEqual({
        output_type: "stream",
        name: "stdout",
        text: "hello world\n",
      });
    });

    it("resolves blob text", async () => {
      const manifest: OutputManifest = {
        output_type: "stream",
        name: "stderr",
        text: { blob: "errhash", size: 100 },
      };

      mockFetch.mockResolvedValueOnce(
        new Response("error output text", { status: 200 }),
      );

      const output = await resolveManifest(manifest, blobPort);
      expect(output).toEqual({
        output_type: "stream",
        name: "stderr",
        text: "error output text",
      });
    });
  });

  describe("error manifests", () => {
    it("resolves traceback from inline ref", async () => {
      const traceback = ["frame1", "frame2", "frame3"];
      const manifest: OutputManifest = {
        output_type: "error",
        ename: "ValueError",
        evalue: "invalid literal",
        traceback: { inline: JSON.stringify(traceback) },
      };

      const output = await resolveManifest(manifest, blobPort);
      expect(output).toEqual({
        output_type: "error",
        ename: "ValueError",
        evalue: "invalid literal",
        traceback: ["frame1", "frame2", "frame3"],
      });
    });

    it("resolves traceback from blob ref", async () => {
      const traceback = ["Traceback (most recent call last):", "  File ..."];
      const manifest: OutputManifest = {
        output_type: "error",
        ename: "RuntimeError",
        evalue: "boom",
        traceback: { blob: "tbhash", size: 200 },
      };

      mockFetch.mockResolvedValueOnce(
        new Response(JSON.stringify(traceback), { status: 200 }),
      );

      const output = await resolveManifest(manifest, blobPort);
      expect(output).toEqual({
        output_type: "error",
        ename: "RuntimeError",
        evalue: "boom",
        traceback,
      });
    });

    it("preserves ename and evalue verbatim", async () => {
      const manifest: OutputManifest = {
        output_type: "error",
        ename: "Custom.Error.Name",
        evalue: "message with 'quotes' and \"doubles\"",
        traceback: { inline: "[]" },
      };

      const output = await resolveManifest(manifest, blobPort);
      if (output.output_type === "error") {
        expect(output.ename).toBe("Custom.Error.Name");
        expect(output.evalue).toBe("message with 'quotes' and \"doubles\"");
      }
    });
  });
});
