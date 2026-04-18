import { describe, expect, it } from "vite-plus/test";
import { frame_types } from "../frame-types";

describe("frame_types constants", () => {
  it("has expected type bytes", () => {
    expect(frame_types.AUTOMERGE_SYNC).toBe(0x00);
    expect(frame_types.REQUEST).toBe(0x01);
    expect(frame_types.RESPONSE).toBe(0x02);
    expect(frame_types.BROADCAST).toBe(0x03);
    expect(frame_types.PRESENCE).toBe(0x04);
    expect(frame_types.RUNTIME_STATE_SYNC).toBe(0x05);
    expect(frame_types.POOL_STATE_SYNC).toBe(0x06);
  });
});
