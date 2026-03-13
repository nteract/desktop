import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  getPeersForCell,
  startCursorDispatch,
  subscribeToCell,
} from "../cursor-registry";
import * as frameBus from "../notebook-frame-bus";

// Mock the frame bus
vi.mock("../notebook-frame-bus", () => ({
  subscribePresence: vi.fn(() => vi.fn()),
}));

// Mock remote-cursors to avoid DOM dependencies
vi.mock("@/components/editor/remote-cursors", () => ({
  peerColor: (peerId: string) => `#${peerId.slice(0, 6)}`,
  setRemoteCursors: vi.fn(),
  setRemoteSelections: vi.fn(),
}));

describe("cursor-registry cell-level functions", () => {
  let cleanup: (() => void) | undefined;
  let presenceHandler: ((payload: unknown) => void) | undefined;

  beforeEach(() => {
    // Capture the presence handler when startCursorDispatch is called
    vi.mocked(frameBus.subscribePresence).mockImplementation((handler) => {
      presenceHandler = handler;
      return vi.fn();
    });

    cleanup = startCursorDispatch("local-peer");
  });

  afterEach(() => {
    cleanup?.();
    presenceHandler = undefined;
    vi.clearAllMocks();
  });

  describe("getPeersForCell", () => {
    it("returns empty array when no peers", () => {
      const peers = getPeersForCell("cell-1");
      expect(peers).toEqual([]);
    });

    it("returns peers with cursors in the specified cell", () => {
      // Simulate a peer cursor update
      presenceHandler?.({
        type: "update",
        peer_id: "peer-1",
        peer_label: "Human",
        channel: "cursor",
        data: { cell_id: "cell-1", line: 0, column: 5 },
      });

      const peers = getPeersForCell("cell-1");
      expect(peers).toHaveLength(1);
      expect(peers[0].peerId).toBe("peer-1");
      expect(peers[0].peerLabel).toBe("Human");
      expect(peers[0].cursor?.cell_id).toBe("cell-1");
    });

    it("excludes peers in other cells", () => {
      presenceHandler?.({
        type: "update",
        peer_id: "peer-1",
        channel: "cursor",
        data: { cell_id: "cell-1", line: 0, column: 0 },
      });
      presenceHandler?.({
        type: "update",
        peer_id: "peer-2",
        channel: "cursor",
        data: { cell_id: "cell-2", line: 0, column: 0 },
      });

      const peersCell1 = getPeersForCell("cell-1");
      const peersCell2 = getPeersForCell("cell-2");

      expect(peersCell1).toHaveLength(1);
      expect(peersCell1[0].peerId).toBe("peer-1");

      expect(peersCell2).toHaveLength(1);
      expect(peersCell2[0].peerId).toBe("peer-2");
    });

    it("excludes local peer", () => {
      // Simulate local peer's cursor (should be filtered out)
      presenceHandler?.({
        type: "update",
        peer_id: "local-peer",
        channel: "cursor",
        data: { cell_id: "cell-1", line: 0, column: 0 },
      });

      const peers = getPeersForCell("cell-1");
      expect(peers).toHaveLength(0);
    });
  });

  describe("subscribeToCell", () => {
    it("notifies subscriber when peer enters cell", () => {
      const callback = vi.fn();
      const unsubscribe = subscribeToCell("cell-1", callback);

      presenceHandler?.({
        type: "update",
        peer_id: "peer-1",
        channel: "cursor",
        data: { cell_id: "cell-1", line: 0, column: 0 },
      });

      expect(callback).toHaveBeenCalledTimes(1);
      unsubscribe();
    });

    it("notifies subscriber when peer leaves cell", () => {
      // Peer enters cell-1
      presenceHandler?.({
        type: "update",
        peer_id: "peer-1",
        channel: "cursor",
        data: { cell_id: "cell-1", line: 0, column: 0 },
      });

      const callback = vi.fn();
      const unsubscribe = subscribeToCell("cell-1", callback);

      // Peer moves to cell-2
      presenceHandler?.({
        type: "update",
        peer_id: "peer-1",
        channel: "cursor",
        data: { cell_id: "cell-2", line: 0, column: 0 },
      });

      // Should notify cell-1 (peer left) and cell-2 (peer entered)
      expect(callback).toHaveBeenCalled();
      unsubscribe();
    });

    it("does not notify after unsubscribe", () => {
      const callback = vi.fn();
      const unsubscribe = subscribeToCell("cell-1", callback);
      unsubscribe();

      presenceHandler?.({
        type: "update",
        peer_id: "peer-1",
        channel: "cursor",
        data: { cell_id: "cell-1", line: 0, column: 0 },
      });

      expect(callback).not.toHaveBeenCalled();
    });

    it("handles multiple subscribers to the same cell", () => {
      const callback1 = vi.fn();
      const callback2 = vi.fn();
      const unsub1 = subscribeToCell("cell-1", callback1);
      const unsub2 = subscribeToCell("cell-1", callback2);

      presenceHandler?.({
        type: "update",
        peer_id: "peer-1",
        channel: "cursor",
        data: { cell_id: "cell-1", line: 0, column: 0 },
      });

      expect(callback1).toHaveBeenCalledTimes(1);
      expect(callback2).toHaveBeenCalledTimes(1);

      unsub1();
      unsub2();
    });

    it("notifies on snapshot message", () => {
      const callback = vi.fn();
      const unsubscribe = subscribeToCell("cell-1", callback);

      presenceHandler?.({
        type: "snapshot",
        peer_id: "daemon",
        peers: [
          {
            peer_id: "peer-1",
            peer_label: "Human",
            channels: [
              {
                channel: "cursor",
                data: { cell_id: "cell-1", line: 0, column: 0 },
              },
            ],
          },
        ],
      });

      expect(callback).toHaveBeenCalled();
      unsubscribe();
    });

    it("notifies on peer left message", () => {
      // First, add a peer
      presenceHandler?.({
        type: "update",
        peer_id: "peer-1",
        channel: "cursor",
        data: { cell_id: "cell-1", line: 0, column: 0 },
      });

      const callback = vi.fn();
      const unsubscribe = subscribeToCell("cell-1", callback);

      // Peer leaves
      presenceHandler?.({
        type: "left",
        peer_id: "peer-1",
      });

      expect(callback).toHaveBeenCalled();
      unsubscribe();
    });
  });
});
