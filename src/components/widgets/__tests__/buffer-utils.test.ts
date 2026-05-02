/**
 * Tests for buffer-utils.ts — base64 + media-src helpers for widgets whose
 * value traitlet carries binary data inline (ipywidgets.Image,
 * ipywidgets.Audio, ipywidgets.Video).
 */

import { describe, expect, it } from "vite-plus/test";
import { arrayBufferToBase64, buildMediaSrc } from "../buffer-utils";

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

    expect(arrayBufferToBase64(buffer)).toBe("AP+A");
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
