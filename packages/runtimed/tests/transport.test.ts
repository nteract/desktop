import { describe, expect, it, vi } from "vite-plus/test";

import {
  FrameType,
  sendAutomergeSyncFrame,
  sendPresenceFrame,
  type NotebookTransport,
} from "../src";

function createTransportStub(): {
  transport: NotebookTransport;
  sendFrame: ReturnType<typeof vi.fn>;
} {
  const sendFrame = vi.fn().mockResolvedValue(undefined);
  const transport = {
    sendFrame,
    onFrame: () => () => {},
    sendRequest: vi.fn(),
    connected: true,
    disconnect: vi.fn(),
  } satisfies NotebookTransport;

  return { transport, sendFrame };
}

describe("FrameType constants", () => {
  it("matches the notebook wire frame discriminants", () => {
    expect(FrameType.AUTOMERGE_SYNC).toBe(0x00);
    expect(FrameType.REQUEST).toBe(0x01);
    expect(FrameType.RESPONSE).toBe(0x02);
    expect(FrameType.BROADCAST).toBe(0x03);
    expect(FrameType.PRESENCE).toBe(0x04);
    expect(FrameType.RUNTIME_STATE_SYNC).toBe(0x05);
    expect(FrameType.POOL_STATE_SYNC).toBe(0x06);
    expect(FrameType.SESSION_CONTROL).toBe(0x07);
  });
});

describe("frame send helpers", () => {
  it("sends Automerge sync payloads with the package-owned frame type", async () => {
    const { transport, sendFrame } = createTransportStub();
    const payload = new Uint8Array([1, 2, 3]);

    await sendAutomergeSyncFrame(transport, payload);

    expect(sendFrame).toHaveBeenCalledWith(FrameType.AUTOMERGE_SYNC, payload);
  });

  it("sends presence payloads with the package-owned frame type", async () => {
    const { transport, sendFrame } = createTransportStub();
    const payload = new Uint8Array([4, 5, 6]);

    await sendPresenceFrame(transport, payload);

    expect(sendFrame).toHaveBeenCalledWith(FrameType.PRESENCE, payload);
  });
});
