/**
 * Tests for buffer-utils.ts - Widget binary data handling utilities.
 *
 * These functions handle the Jupyter widget protocol's binary buffer transport,
 * which is critical for widgets that work with images, audio, and other binary data.
 */

import { describe, expect, it } from "vite-plus/test";
import {
  applyBufferPaths,
  arrayBufferToBase64,
  buildMediaSrc,
  extractBuffers,
  findBufferPaths,
} from "../buffer-utils";

describe("applyBufferPaths", () => {
  it("returns original data when bufferPaths is undefined", () => {
    const data = { foo: "bar" };
    const result = applyBufferPaths(data, undefined, [new ArrayBuffer(8)]);
    expect(result).toBe(data);
    expect(result).toEqual({ foo: "bar" });
  });

  it("returns original data when buffers is undefined", () => {
    const data = { foo: "bar" };
    const result = applyBufferPaths(data, [["foo"]], undefined);
    expect(result).toBe(data);
    expect(result).toEqual({ foo: "bar" });
  });

  it("returns original data when bufferPaths is empty", () => {
    const data = { foo: "bar" };
    const result = applyBufferPaths(data, [], [new ArrayBuffer(8)]);
    expect(result).toBe(data);
  });

  it("applies buffer at single-level path", () => {
    const data = { value: null };
    const buffer = new ArrayBuffer(8);
    const result = applyBufferPaths(data, [["value"]], [buffer]);
    expect(result.value).toBe(buffer);
  });

  it("applies buffer at nested path", () => {
    const data = { nested: { deep: { value: null } } };
    const buffer = new ArrayBuffer(8);
    const result = applyBufferPaths(data, [["nested", "deep", "value"]], [buffer]);
    expect((result.nested as Record<string, unknown>).deep).toEqual({
      value: buffer,
    });
  });

  it("creates intermediate objects for missing paths", () => {
    const data: Record<string, unknown> = {};
    const buffer = new ArrayBuffer(8);
    const result = applyBufferPaths(data, [["a", "b", "c"]], [buffer]);
    expect(result.a).toBeDefined();
    expect((result.a as Record<string, unknown>).b).toBeDefined();
    expect(((result.a as Record<string, unknown>).b as Record<string, unknown>).c).toBe(buffer);
  });

  it("applies multiple buffers to multiple paths", () => {
    const data = { first: null, second: null };
    const buffer1 = new ArrayBuffer(4);
    const buffer2 = new ArrayBuffer(8);
    const result = applyBufferPaths(data, [["first"], ["second"]], [buffer1, buffer2]);
    expect(result.first).toBe(buffer1);
    expect(result.second).toBe(buffer2);
  });

  it("handles mismatched buffer count (fewer buffers than paths)", () => {
    const data = { first: null, second: null };
    const buffer = new ArrayBuffer(4);
    const result = applyBufferPaths(data, [["first"], ["second"]], [buffer]);
    expect(result.first).toBe(buffer);
    expect(result.second).toBeNull();
  });

  it("skips empty paths", () => {
    const data = { value: "original" };
    const buffer = new ArrayBuffer(8);
    const result = applyBufferPaths(data, [[]], [buffer]);
    expect(result).toEqual({ value: "original" });
  });

  it("overwrites existing values", () => {
    const data = { value: "will be replaced" };
    const buffer = new ArrayBuffer(8);
    const result = applyBufferPaths(data, [["value"]], [buffer]);
    expect(result.value).toBe(buffer);
  });
});

describe("extractBuffers", () => {
  it("returns empty array when bufferPaths is undefined", () => {
    const data = { buffer: new ArrayBuffer(8) };
    const result = extractBuffers(data, undefined);
    expect(result).toEqual([]);
  });

  it("returns empty array when bufferPaths is empty", () => {
    const data = { buffer: new ArrayBuffer(8) };
    const result = extractBuffers(data, []);
    expect(result).toEqual([]);
  });

  it("extracts ArrayBuffer from single-level path", () => {
    const buffer = new ArrayBuffer(8);
    const data: Record<string, unknown> = { value: buffer };
    const result = extractBuffers(data, [["value"]]);
    expect(result).toHaveLength(1);
    expect(result[0]).toBe(buffer);
    expect(data.value).toBeNull();
  });

  it("extracts ArrayBuffer from nested path", () => {
    const buffer = new ArrayBuffer(8);
    const data = { nested: { deep: { value: buffer } } };
    const result = extractBuffers(data, [["nested", "deep", "value"]]);
    expect(result).toHaveLength(1);
    expect(result[0]).toBe(buffer);
    expect((data.nested.deep as Record<string, unknown>).value).toBeNull();
  });

  it("handles Uint8Array by slicing its buffer", () => {
    const originalBuffer = new ArrayBuffer(16);
    const view = new Uint8Array(originalBuffer, 4, 8); // offset 4, length 8
    view[0] = 42;
    view[7] = 99;

    const data: Record<string, unknown> = { value: view };
    const result = extractBuffers(data, [["value"]]);

    expect(result).toHaveLength(1);
    expect(result[0].byteLength).toBe(8);
    const extractedView = new Uint8Array(result[0]);
    expect(extractedView[0]).toBe(42);
    expect(extractedView[7]).toBe(99);
    expect(data.value).toBeNull();
  });

  it("handles other typed arrays (Int32Array)", () => {
    const buffer = new ArrayBuffer(16);
    const view = new Int32Array(buffer);
    view[0] = 12345;

    const data: Record<string, unknown> = { value: view };
    const result = extractBuffers(data, [["value"]]);

    expect(result).toHaveLength(1);
    expect(result[0].byteLength).toBe(16);
    expect(data.value).toBeNull();
  });

  it("returns empty ArrayBuffer for path that does not exist", () => {
    const data = { other: "value" };
    const result = extractBuffers(data, [["missing"]]);
    expect(result).toHaveLength(1);
    expect(result[0].byteLength).toBe(0);
  });

  it("returns empty ArrayBuffer for path with non-buffer value", () => {
    const data = { value: "not a buffer" };
    const result = extractBuffers(data, [["value"]]);
    expect(result).toHaveLength(1);
    expect(result[0].byteLength).toBe(0);
  });

  it("returns empty ArrayBuffer for empty path", () => {
    const data = { value: new ArrayBuffer(8) };
    const result = extractBuffers(data, [[]]);
    expect(result).toHaveLength(1);
    expect(result[0].byteLength).toBe(0);
  });

  it("extracts multiple buffers from multiple paths", () => {
    const buffer1 = new ArrayBuffer(4);
    const buffer2 = new ArrayBuffer(8);
    const data: Record<string, unknown> = { first: buffer1, second: buffer2 };

    const result = extractBuffers(data, [["first"], ["second"]]);

    expect(result).toHaveLength(2);
    expect(result[0]).toBe(buffer1);
    expect(result[1]).toBe(buffer2);
    expect(data.first).toBeNull();
    expect(data.second).toBeNull();
  });

  it("handles intermediate null in path", () => {
    const data = { nested: null };
    const result = extractBuffers(data, [["nested", "deep", "value"]]);
    expect(result).toHaveLength(1);
    expect(result[0].byteLength).toBe(0);
  });

  it("handles intermediate non-object in path", () => {
    const data = { nested: "string" };
    const result = extractBuffers(data, [["nested", "deep", "value"]]);
    expect(result).toHaveLength(1);
    expect(result[0].byteLength).toBe(0);
  });
});

describe("arrayBufferToBase64", () => {
  it("returns empty string for empty ArrayBuffer", () => {
    const buffer = new ArrayBuffer(0);
    expect(arrayBufferToBase64(buffer)).toBe("");
  });

  it("converts known bytes to correct base64", () => {
    // "Hello" in ASCII is [72, 101, 108, 108, 111]
    const buffer = new ArrayBuffer(5);
    const view = new Uint8Array(buffer);
    view.set([72, 101, 108, 108, 111]);

    expect(arrayBufferToBase64(buffer)).toBe("SGVsbG8=");
  });

  it("handles Uint8Array input directly", () => {
    const view = new Uint8Array([72, 101, 108, 108, 111]);
    expect(arrayBufferToBase64(view)).toBe("SGVsbG8=");
  });

  it("handles binary data with all byte values", () => {
    const buffer = new ArrayBuffer(3);
    const view = new Uint8Array(buffer);
    view.set([0, 255, 128]);

    const result = arrayBufferToBase64(buffer);
    expect(result).toBe("AP+A");
  });

  it("produces correct base64 for single byte", () => {
    const view = new Uint8Array([65]); // 'A'
    expect(arrayBufferToBase64(view)).toBe("QQ==");
  });

  it("produces correct base64 for two bytes", () => {
    const view = new Uint8Array([65, 66]); // 'AB'
    expect(arrayBufferToBase64(view)).toBe("QUI=");
  });

  it("produces correct base64 for three bytes (no padding)", () => {
    const view = new Uint8Array([65, 66, 67]); // 'ABC'
    expect(arrayBufferToBase64(view)).toBe("QUJD");
  });
});

describe("buildMediaSrc", () => {
  it("returns undefined for null value", () => {
    expect(buildMediaSrc(null, "image", "png")).toBeUndefined();
  });

  it("returns undefined for undefined value", () => {
    expect(buildMediaSrc(undefined, "image", "png")).toBeUndefined();
  });

  it("returns undefined for empty string", () => {
    expect(buildMediaSrc("", "image", "png")).toBeUndefined();
  });

  it("converts ArrayBuffer to data URL", () => {
    const buffer = new ArrayBuffer(5);
    const view = new Uint8Array(buffer);
    view.set([72, 101, 108, 108, 111]);

    const result = buildMediaSrc(buffer, "image", "png");
    expect(result).toBe("data:image/png;base64,SGVsbG8=");
  });

  it("converts Uint8Array to data URL", () => {
    const view = new Uint8Array([72, 101, 108, 108, 111]);

    const result = buildMediaSrc(view, "audio", "wav");
    expect(result).toBe("data:audio/wav;base64,SGVsbG8=");
  });

  it("passes through data URLs unchanged", () => {
    const dataUrl = "data:image/png;base64,ABC123";
    expect(buildMediaSrc(dataUrl, "image", "jpeg")).toBe(dataUrl);
  });

  it("passes through http URLs unchanged", () => {
    const url = "http://example.com/image.png";
    expect(buildMediaSrc(url, "image", "png")).toBe(url);
  });

  it("passes through https URLs unchanged", () => {
    const url = "https://example.com/image.png";
    expect(buildMediaSrc(url, "image", "png")).toBe(url);
  });

  it("passes through absolute paths unchanged", () => {
    const path = "/assets/image.png";
    expect(buildMediaSrc(path, "image", "png")).toBe(path);
  });

  it("wraps plain base64 string in data URL", () => {
    const base64 = "SGVsbG8=";
    const result = buildMediaSrc(base64, "image", "gif");
    expect(result).toBe("data:image/gif;base64,SGVsbG8=");
  });

  it("uses correct media type and format in data URL", () => {
    const view = new Uint8Array([1, 2, 3]);
    const result = buildMediaSrc(view, "video", "mp4");
    expect(result).toMatch(/^data:video\/mp4;base64,/);
  });
});

describe("findBufferPaths", () => {
  it("returns empty array for object with no buffers", () => {
    const data = { foo: "bar", num: 42, nested: { value: true } };
    expect(findBufferPaths(data)).toEqual([]);
  });

  it("finds ArrayBuffer at top level", () => {
    const data = { buffer: new ArrayBuffer(8), other: "value" };
    const paths = findBufferPaths(data);
    expect(paths).toEqual([["buffer"]]);
  });

  it("finds Uint8Array at top level", () => {
    const data = { buffer: new Uint8Array(8), other: "value" };
    const paths = findBufferPaths(data);
    expect(paths).toEqual([["buffer"]]);
  });

  it("finds other typed arrays (Int32Array)", () => {
    const data = { buffer: new Int32Array(4) };
    const paths = findBufferPaths(data);
    expect(paths).toEqual([["buffer"]]);
  });

  it("finds ArrayBuffer in nested object", () => {
    const data = { nested: { deep: { buffer: new ArrayBuffer(8) } } };
    const paths = findBufferPaths(data);
    expect(paths).toEqual([["nested", "deep", "buffer"]]);
  });

  it("finds multiple buffers at various depths", () => {
    const data = {
      top: new ArrayBuffer(4),
      nested: {
        middle: new Uint8Array(8),
        deeper: {
          bottom: new ArrayBuffer(16),
        },
      },
    };
    const paths = findBufferPaths(data);
    expect(paths).toHaveLength(3);
    expect(paths).toContainEqual(["top"]);
    expect(paths).toContainEqual(["nested", "middle"]);
    expect(paths).toContainEqual(["nested", "deeper", "bottom"]);
  });

  it("does not recurse into arrays", () => {
    const data = {
      array: [new ArrayBuffer(8)],
      buffer: new ArrayBuffer(4),
    };
    const paths = findBufferPaths(data);
    // Should only find 'buffer', not the one inside the array
    expect(paths).toEqual([["buffer"]]);
  });

  it("handles empty object", () => {
    expect(findBufferPaths({})).toEqual([]);
  });

  it("ignores null values", () => {
    const data = { nullValue: null, buffer: new ArrayBuffer(8) };
    const paths = findBufferPaths(data);
    expect(paths).toEqual([["buffer"]]);
  });

  it("uses provided prefix for paths", () => {
    const data = { buffer: new ArrayBuffer(8) };
    const paths = findBufferPaths(data, ["prefix", "path"]);
    expect(paths).toEqual([["prefix", "path", "buffer"]]);
  });
});

describe("roundtrip: applyBufferPaths and extractBuffers", () => {
  it("extracting then applying restores original structure", () => {
    const buffer = new ArrayBuffer(8);
    const view = new Uint8Array(buffer);
    view.set([1, 2, 3, 4, 5, 6, 7, 8]);

    const original: Record<string, unknown> = {
      nested: { data: buffer },
      other: "value",
    };
    const paths = [["nested", "data"]];

    // Extract (mutates original, sets to null)
    const extractedBuffers = extractBuffers(original, paths);
    expect(original.nested).toEqual({ data: null });

    // Apply to restore
    const restored = applyBufferPaths(original, paths, extractedBuffers);
    expect(restored.nested).toEqual({ data: extractedBuffers[0] });

    // Verify buffer contents preserved
    const restoredView = new Uint8Array((restored.nested as Record<string, ArrayBuffer>).data);
    expect(Array.from(restoredView)).toEqual([1, 2, 3, 4, 5, 6, 7, 8]);
  });

  it("findBufferPaths produces paths usable by extractBuffers", () => {
    const data: Record<string, unknown> = {
      image: new ArrayBuffer(100),
      nested: {
        audio: new Uint8Array(50),
      },
    };

    const paths = findBufferPaths(data);
    expect(paths).toHaveLength(2);

    const buffers = extractBuffers(data, paths);
    expect(buffers).toHaveLength(2);
    expect(data.image).toBeNull();
    expect((data.nested as Record<string, unknown>).audio).toBeNull();
  });
});
