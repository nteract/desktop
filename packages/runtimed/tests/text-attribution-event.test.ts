import { describe, expect, it } from "vite-plus/test";
import {
  createTextAttributionEvent,
  isTextAttributionEvent,
  TEXT_ATTRIBUTION_EVENT_TYPE,
} from "../src/text-attribution-event";

const ATTRIBUTION = {
  cell_id: "cell-1",
  index: 3,
  text: "abc",
  deleted: 0,
  actors: ["human:local"],
};

describe("text attribution event contract", () => {
  it("creates the shared text attribution broadcast shape", () => {
    expect(createTextAttributionEvent([ATTRIBUTION])).toEqual({
      type: TEXT_ATTRIBUTION_EVENT_TYPE,
      attributions: [ATTRIBUTION],
    });
  });

  it("accepts valid text attribution events", () => {
    expect(isTextAttributionEvent(createTextAttributionEvent([ATTRIBUTION]))).toBe(true);
    expect(isTextAttributionEvent(createTextAttributionEvent([]))).toBe(true);
  });

  it.each([
    ["null", null],
    ["array", []],
    ["wrong type", { type: "presence", attributions: [ATTRIBUTION] }],
    ["missing attributions", { type: TEXT_ATTRIBUTION_EVENT_TYPE }],
    ["non-array attributions", { type: TEXT_ATTRIBUTION_EVENT_TYPE, attributions: ATTRIBUTION }],
    [
      "invalid cell_id",
      { type: TEXT_ATTRIBUTION_EVENT_TYPE, attributions: [{ ...ATTRIBUTION, cell_id: 1 }] },
    ],
    [
      "invalid index",
      { type: TEXT_ATTRIBUTION_EVENT_TYPE, attributions: [{ ...ATTRIBUTION, index: "3" }] },
    ],
    [
      "invalid text",
      { type: TEXT_ATTRIBUTION_EVENT_TYPE, attributions: [{ ...ATTRIBUTION, text: null }] },
    ],
    [
      "invalid deleted",
      { type: TEXT_ATTRIBUTION_EVENT_TYPE, attributions: [{ ...ATTRIBUTION, deleted: NaN }] },
    ],
    [
      "invalid actors",
      { type: TEXT_ATTRIBUTION_EVENT_TYPE, attributions: [{ ...ATTRIBUTION, actors: [42] }] },
    ],
  ])("rejects %s", (_label, payload) => {
    expect(isTextAttributionEvent(payload)).toBe(false);
  });
});
