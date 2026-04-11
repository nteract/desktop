import { describe, expect, it } from "vite-plus/test";
import { detectRawFormat } from "../detect-raw-format";

describe("detectRawFormat", () => {
  describe("metadata.format takes precedence", () => {
    it("respects metadata.format = yaml", () => {
      expect(detectRawFormat("key = value", { format: "yaml" })).toBe("yaml");
    });

    it("respects metadata.format = yml", () => {
      expect(detectRawFormat("key = value", { format: "yml" })).toBe("yaml");
    });

    it("respects metadata.format = toml", () => {
      expect(detectRawFormat("key: value", { format: "toml" })).toBe("toml");
    });

    it("respects metadata.format = json", () => {
      expect(detectRawFormat("key: value", { format: "json" })).toBe("json");
    });

    it("is case insensitive", () => {
      expect(detectRawFormat("content", { format: "YAML" })).toBe("yaml");
      expect(detectRawFormat("content", { format: "TOML" })).toBe("toml");
    });
  });

  describe("JSON detection", () => {
    it("detects object JSON", () => {
      expect(detectRawFormat('{"key": "value"}')).toBe("json");
    });

    it("detects array JSON", () => {
      expect(detectRawFormat('["a", "b", "c"]')).toBe("json");
    });

    it("detects JSON with leading whitespace", () => {
      expect(detectRawFormat('  \n  {"key": "value"}')).toBe("json");
    });
  });

  describe("YAML detection", () => {
    it("detects YAML frontmatter delimiter", () => {
      expect(detectRawFormat("---\ntitle: Test\n---")).toBe("yaml");
    });

    it("detects key: value patterns", () => {
      expect(detectRawFormat("title: My Document\nauthor: John")).toBe("yaml");
    });

    it("does not detect URLs as YAML", () => {
      // URL patterns like http: should not trigger YAML detection
      expect(detectRawFormat("http://example.com")).toBe("plain");
    });
  });

  describe("TOML detection", () => {
    it("detects [section] headers", () => {
      expect(detectRawFormat("[package]\nname = 'test'")).toBe("toml");
    });

    it("detects key = value patterns", () => {
      expect(detectRawFormat("name = 'test'\nversion = '1.0'")).toBe("toml");
    });

    it("detects dotted section headers", () => {
      expect(detectRawFormat("[tool.poetry]\nname = 'test'")).toBe("toml");
    });
  });

  describe("plain text fallback", () => {
    it("returns plain for empty content", () => {
      expect(detectRawFormat("")).toBe("plain");
    });

    it("returns plain for whitespace-only content", () => {
      expect(detectRawFormat("   \n\t  ")).toBe("plain");
    });

    it("returns plain for unstructured text", () => {
      expect(detectRawFormat("Just some plain text content")).toBe("plain");
    });
  });
});
