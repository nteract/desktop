import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import {
  findPeerColorByActorLabel,
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

  // ── Actor label identity alignment tests ────────────────────────

  describe("findPeerColorByActorLabel", () => {
    it("returns color for exact actor_label match", () => {
      presenceHandler?.({
        type: "update",
        peer_id: "peer-1",
        peer_label: "Claude",
        actor_label: "agent:claude:abc123",
        channel: "cursor",
        data: { cell_id: "cell-1", line: 0, column: 0 },
      });

      const color = findPeerColorByActorLabel("agent:claude:abc123");
      expect(color).toBeDefined();
      // peerColor mock returns `#${peerId.slice(0, 6)}`
      expect(color).toBe("#peer-1");
    });

    it("returns undefined when no peer matches actor_label", () => {
      presenceHandler?.({
        type: "update",
        peer_id: "peer-1",
        peer_label: "Claude",
        actor_label: "agent:claude:abc123",
        channel: "cursor",
        data: { cell_id: "cell-1", line: 0, column: 0 },
      });

      const color = findPeerColorByActorLabel("agent:other:xyz789");
      expect(color).toBeUndefined();
    });

    it("returns undefined when no peers are connected", () => {
      const color = findPeerColorByActorLabel("agent:claude:abc123");
      expect(color).toBeUndefined();
    });

    it("does not match against the local peer", () => {
      // local-peer is the ID passed to startCursorDispatch
      presenceHandler?.({
        type: "update",
        peer_id: "local-peer",
        peer_label: "Me",
        actor_label: "human:local-session",
        channel: "cursor",
        data: { cell_id: "cell-1", line: 0, column: 0 },
      });

      const color = findPeerColorByActorLabel("human:local-session");
      expect(color).toBeUndefined();
    });

    it("matches actor_label from snapshot", () => {
      presenceHandler?.({
        type: "snapshot",
        peer_id: "daemon",
        peers: [
          {
            peer_id: "agent-peer",
            peer_label: "Claude",
            actor_label: "agent:claude:snap123",
            channels: [
              {
                channel: "cursor",
                data: { cell_id: "cell-1", line: 0, column: 0 },
              },
            ],
          },
        ],
      });

      const color = findPeerColorByActorLabel("agent:claude:snap123");
      expect(color).toBeDefined();
      expect(color).toBe("#agent-");
    });

    it("loses actor_label when peer leaves", () => {
      presenceHandler?.({
        type: "update",
        peer_id: "peer-1",
        peer_label: "Claude",
        actor_label: "agent:claude:abc123",
        channel: "cursor",
        data: { cell_id: "cell-1", line: 0, column: 0 },
      });

      // Verify it's there
      expect(findPeerColorByActorLabel("agent:claude:abc123")).toBeDefined();

      // Peer disconnects
      presenceHandler?.({
        type: "left",
        peer_id: "peer-1",
      });

      // Actor label lookup should no longer match
      expect(findPeerColorByActorLabel("agent:claude:abc123")).toBeUndefined();
    });

    it("does not match by substring (exact only)", () => {
      presenceHandler?.({
        type: "update",
        peer_id: "peer-1",
        peer_label: "Claude",
        actor_label: "agent:claude:abc123",
        channel: "cursor",
        data: { cell_id: "cell-1", line: 0, column: 0 },
      });

      // Partial match should NOT work — this is the key improvement over
      // the old fuzzy findPeerColorByLabel
      expect(findPeerColorByActorLabel("agent:claude")).toBeUndefined();
      expect(findPeerColorByActorLabel("claude")).toBeUndefined();
      expect(findPeerColorByActorLabel("abc123")).toBeUndefined();
    });
  });

  describe("actor_label storage in PeerCursorInfo", () => {
    it("stores actor_label from update messages", () => {
      presenceHandler?.({
        type: "update",
        peer_id: "peer-1",
        peer_label: "Claude",
        actor_label: "agent:claude:abc123",
        channel: "cursor",
        data: { cell_id: "cell-1", line: 0, column: 0 },
      });

      const peers = getPeersForCell("cell-1");
      expect(peers).toHaveLength(1);
      expect(peers[0].actorLabel).toBe("agent:claude:abc123");
    });

    it("stores actor_label from snapshot", () => {
      presenceHandler?.({
        type: "snapshot",
        peer_id: "daemon",
        peers: [
          {
            peer_id: "peer-1",
            peer_label: "Human",
            actor_label: "human:session-abc",
            channels: [
              {
                channel: "cursor",
                data: { cell_id: "cell-1", line: 0, column: 0 },
              },
            ],
          },
        ],
      });

      const peers = getPeersForCell("cell-1");
      expect(peers).toHaveLength(1);
      expect(peers[0].actorLabel).toBe("human:session-abc");
    });

    it("preserves actor_label when absent from subsequent updates", () => {
      // First update includes actor_label
      presenceHandler?.({
        type: "update",
        peer_id: "peer-1",
        peer_label: "Claude",
        actor_label: "agent:claude:abc123",
        channel: "cursor",
        data: { cell_id: "cell-1", line: 0, column: 0 },
      });

      // Second update without actor_label (e.g. just a cursor move)
      presenceHandler?.({
        type: "update",
        peer_id: "peer-1",
        peer_label: "Claude",
        channel: "cursor",
        data: { cell_id: "cell-1", line: 5, column: 10 },
      });

      const peers = getPeersForCell("cell-1");
      expect(peers).toHaveLength(1);
      expect(peers[0].actorLabel).toBe("agent:claude:abc123");
    });

    it("handles peer with no actor_label gracefully", () => {
      presenceHandler?.({
        type: "update",
        peer_id: "peer-1",
        peer_label: "Old Client",
        channel: "cursor",
        data: { cell_id: "cell-1", line: 0, column: 0 },
      });

      const peers = getPeersForCell("cell-1");
      expect(peers).toHaveLength(1);
      expect(peers[0].actorLabel).toBeUndefined();

      // And the lookup should return undefined
      expect(findPeerColorByActorLabel("anything")).toBeUndefined();
    });
  });
});
