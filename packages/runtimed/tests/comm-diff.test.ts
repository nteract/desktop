/**
 * Tests for comm-diff output detection logic.
 *
 * Covers:
 * - Legacy hash string detection (backward compat)
 * - Inline manifest object detection (new format)
 * - Mixed arrays (hashes + manifest objects)
 * - Negative cases (non-OutputModel, empty, already-resolved)
 * - Deprecated detectOutputManifestHashes still works
 */

import { describe, expect, it } from "vite-plus/test";
import {
  detectOutputManifestHashes,
  detectUnresolvedOutputs,
  isManifestHash,
} from "../src/comm-diff";

// ── isManifestHash ──────────────────────────────────────────────────

describe("isManifestHash", () => {
  it("accepts a 64-char lowercase hex string", () => {
    const hash = "a".repeat(64);
    expect(isManifestHash(hash)).toBe(true);
  });

  it("accepts a realistic SHA-256 hex digest", () => {
    expect(isManifestHash("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")).toBe(
      true,
    );
  });

  it("rejects strings shorter than 64 chars", () => {
    expect(isManifestHash("abcdef1234")).toBe(false);
  });

  it("rejects strings longer than 64 chars", () => {
    expect(isManifestHash("a".repeat(65))).toBe(false);
  });

  it("rejects uppercase hex", () => {
    expect(isManifestHash("A".repeat(64))).toBe(false);
  });

  it("rejects non-hex characters", () => {
    expect(isManifestHash("g".repeat(64))).toBe(false);
  });
});

// ── detectUnresolvedOutputs ─────────────────────────────────────────

describe("detectUnresolvedOutputs", () => {
  const hash1 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
  const hash2 = "a".repeat(64);

  const manifestObj = {
    output_type: "display_data",
    data: { "text/plain": { inline: "hello" } },
    metadata: {},
  };

  const executeResultObj = {
    output_type: "execute_result",
    data: { "text/plain": { inline: "42" } },
    metadata: {},
    execution_count: 1,
  };

  // ── Positive cases ──────────────────────────────────────────────

  it("detects hash strings in OutputModel", () => {
    const result = detectUnresolvedOutputs({
      _model_name: "OutputModel",
      outputs: [hash1, hash2],
    });
    expect(result).not.toBeNull();
    expect(result!.outputs).toEqual([hash1, hash2]);
  });

  it("detects inline manifest objects in OutputModel", () => {
    const result = detectUnresolvedOutputs({
      _model_name: "OutputModel",
      outputs: [manifestObj],
    });
    expect(result).not.toBeNull();
    expect(result!.outputs).toEqual([manifestObj]);
  });

  it("detects mixed hashes and manifest objects", () => {
    const result = detectUnresolvedOutputs({
      _model_name: "OutputModel",
      outputs: [hash1, manifestObj, executeResultObj],
    });
    expect(result).not.toBeNull();
    expect(result!.outputs).toHaveLength(3);
    expect(result!.outputs[0]).toBe(hash1);
    expect(result!.outputs[1]).toBe(manifestObj);
    expect(result!.outputs[2]).toBe(executeResultObj);
  });

  it("detects a single manifest object", () => {
    const result = detectUnresolvedOutputs({
      _model_name: "OutputModel",
      outputs: [executeResultObj],
    });
    expect(result).not.toBeNull();
    expect(result!.outputs).toEqual([executeResultObj]);
  });

  // ── Negative cases ──────────────────────────────────────────────

  it("returns null for non-OutputModel", () => {
    expect(
      detectUnresolvedOutputs({
        _model_name: "SliderModel",
        outputs: [hash1],
      }),
    ).toBeNull();
  });

  it("returns null when _model_name is missing", () => {
    expect(detectUnresolvedOutputs({ outputs: [hash1] })).toBeNull();
  });

  it("returns null for empty outputs array", () => {
    expect(
      detectUnresolvedOutputs({
        _model_name: "OutputModel",
        outputs: [],
      }),
    ).toBeNull();
  });

  it("returns null when outputs is not an array", () => {
    expect(
      detectUnresolvedOutputs({
        _model_name: "OutputModel",
        outputs: "not-an-array",
      }),
    ).toBeNull();
  });

  it("returns null when outputs is undefined", () => {
    expect(
      detectUnresolvedOutputs({
        _model_name: "OutputModel",
      }),
    ).toBeNull();
  });

  it("returns null for already-resolved outputs (plain objects without output_type)", () => {
    expect(
      detectUnresolvedOutputs({
        _model_name: "OutputModel",
        outputs: [{ data: { "text/plain": "hello" }, metadata: {} }],
      }),
    ).toBeNull();
  });

  it("returns null when array contains non-hash strings", () => {
    expect(
      detectUnresolvedOutputs({
        _model_name: "OutputModel",
        outputs: ["not-a-hash"],
      }),
    ).toBeNull();
  });

  it("returns null for mixed valid and invalid entries", () => {
    expect(
      detectUnresolvedOutputs({
        _model_name: "OutputModel",
        outputs: [hash1, 42],
      }),
    ).toBeNull();
  });

  it("returns null when manifest object has non-string output_type", () => {
    expect(
      detectUnresolvedOutputs({
        _model_name: "OutputModel",
        outputs: [{ output_type: 123 }],
      }),
    ).toBeNull();
  });
});

// ── detectOutputManifestHashes (deprecated, backward compat) ────────

describe("detectOutputManifestHashes (deprecated)", () => {
  const hash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

  it("still detects hash strings", () => {
    const result = detectOutputManifestHashes({
      _model_name: "OutputModel",
      outputs: [hash],
    });
    expect(result).not.toBeNull();
    expect(result!.hashes).toEqual([hash]);
  });

  it("returns null for inline manifest objects (hash-only detection)", () => {
    // The old function only recognizes hash strings, not manifest objects
    const result = detectOutputManifestHashes({
      _model_name: "OutputModel",
      outputs: [
        {
          output_type: "display_data",
          data: { "text/plain": { inline: "hello" } },
          metadata: {},
        },
      ],
    });
    expect(result).toBeNull();
  });

  it("returns null for non-OutputModel", () => {
    expect(
      detectOutputManifestHashes({
        _model_name: "SliderModel",
        outputs: [hash],
      }),
    ).toBeNull();
  });
});
