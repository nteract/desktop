/**
 * Tests for JSON-RPC message handling in the bootstrap HTML.
 *
 * The bootstrap HTML's inline script handles both legacy { type, payload }
 * and JSON-RPC 2.0 { jsonrpc: "2.0", method, params } formats. These tests
 * verify the JSON-RPC routing path works correctly.
 */

import { describe, expect, it } from "vitest";
import { generateFrameHtml } from "../frame-html";

describe("bootstrap HTML JSON-RPC support", () => {
  const html = generateFrameHtml({ darkMode: true });

  it("checks for jsonrpc 2.0 format before legacy format", () => {
    // The handler should check data.jsonrpc === '2.0' early
    expect(html).toContain("data.jsonrpc === '2.0'");
  });

  it("routes nteract/search to handleSearch", () => {
    expect(html).toContain("case 'nteract/search':");
    expect(html).toContain("handleSearch(params)");
  });

  it("routes nteract/searchNavigate to handleSearchNavigate", () => {
    expect(html).toContain("case 'nteract/searchNavigate':");
    expect(html).toContain("handleSearchNavigate(params)");
  });

  it("routes nteract/eval to handleEval", () => {
    expect(html).toContain("case 'nteract/eval':");
    expect(html).toContain("handleEval(params)");
  });

  it("routes nteract/renderOutput with React gate", () => {
    expect(html).toContain("case 'nteract/renderOutput':");
  });

  it("routes nteract/theme with React gate", () => {
    expect(html).toContain("case 'nteract/theme':");
  });

  it("routes nteract/clearOutputs with React gate", () => {
    expect(html).toContain("case 'nteract/clearOutputs':");
  });

  it("routes nteract/ping to handlePing", () => {
    expect(html).toContain("case 'nteract/ping':");
  });

  it("sends outgoing messages in JSON-RPC format via sendRpc", () => {
    // sendRpc helper should produce { jsonrpc: '2.0', method, params }
    expect(html).toContain("function sendRpc(method, params)");
    expect(html).toContain("jsonrpc: '2.0'");
  });

  it("sends ready in legacy format via sendLegacy", () => {
    // Ready must stay legacy (host creates transport in response)
    expect(html).toContain("sendLegacy('ready'");
    expect(html).toContain("function sendLegacy(type, payload)");
  });

  it("sends search_results as JSON-RPC", () => {
    expect(html).toContain("sendRpc('nteract/searchResults'");
  });

  it("sends render_complete as JSON-RPC", () => {
    expect(html).toContain("sendRpc('nteract/renderComplete'");
  });

  it("sends resize as JSON-RPC", () => {
    expect(html).toContain("sendRpc('nteract/resize'");
  });

  it("sends link_click as JSON-RPC", () => {
    expect(html).toContain("sendRpc('nteract/linkClick'");
  });

  it("sends error as JSON-RPC", () => {
    expect(html).toContain("sendRpc('nteract/error'");
  });

  it("sends eval_result as JSON-RPC", () => {
    expect(html).toContain("sendRpc('nteract/evalResult'");
  });

  it("preserves legacy handler as fallback", () => {
    // Legacy switch should still exist after the JSON-RPC block
    expect(html).toContain("case 'search':");
    expect(html).toContain("case 'eval':");
    expect(html).toContain("case 'render':");
  });
});
