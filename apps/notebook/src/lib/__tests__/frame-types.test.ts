// @vitest-environment jsdom
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { frame_types, sendFrame } from "../frame-types";

const mockInvoke = vi.fn();

beforeEach(() => {
  mockIPC((cmd, args) => mockInvoke(cmd, args));
});

afterEach(() => {
  mockInvoke.mockReset();
  clearMocks();
});

describe("frame_types constants", () => {
  it("has expected type bytes", () => {
    expect(frame_types.AUTOMERGE_SYNC).toBe(0x00);
    expect(frame_types.REQUEST).toBe(0x01);
    expect(frame_types.RESPONSE).toBe(0x02);
    expect(frame_types.BROADCAST).toBe(0x03);
    expect(frame_types.PRESENCE).toBe(0x04);
  });
});

describe("sendFrame", () => {
  it("prepends the frame type byte to the payload", async () => {
    mockInvoke.mockResolvedValueOnce(undefined);
    const payload = new Uint8Array([0x10, 0x20, 0x30]);

    await sendFrame(frame_types.REQUEST, payload);

    expect(mockInvoke).toHaveBeenCalledTimes(1);
    const [cmd] = mockInvoke.mock.calls[0];
    expect(cmd).toBe("send_frame");
  });

  it("sends an AUTOMERGE_SYNC frame", async () => {
    mockInvoke.mockResolvedValueOnce(undefined);
    const syncMsg = new Uint8Array([0xaa, 0xbb]);

    await sendFrame(frame_types.AUTOMERGE_SYNC, syncMsg);

    expect(mockInvoke).toHaveBeenCalledWith("send_frame", expect.anything());
  });

  it("handles empty payload", async () => {
    mockInvoke.mockResolvedValueOnce(undefined);
    const empty = new Uint8Array(0);

    await sendFrame(frame_types.PRESENCE, empty);

    expect(mockInvoke).toHaveBeenCalledTimes(1);
  });

  it("propagates invoke errors", async () => {
    mockInvoke.mockRejectedValueOnce(new Error("connection lost"));

    await expect(
      sendFrame(frame_types.BROADCAST, new Uint8Array([1])),
    ).rejects.toThrow("connection lost");
  });
});
