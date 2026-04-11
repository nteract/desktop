/**
 * Cell-level presence indicators.
 *
 * Shows colored dots for remote peers that have their cursor in this cell.
 * Uses the cursor registry's subscription mechanism for efficient updates.
 */

import { useEffect, useState } from "react";
import { getPeersForCell, type PeerCursorInfo, subscribeToCell } from "../../lib/cursor-registry";

interface CellPresenceIndicatorsProps {
  cellId: string;
}

/** Maximum visible indicators before showing overflow count */
const MAX_VISIBLE = 3;

export function CellPresenceIndicators({ cellId }: CellPresenceIndicatorsProps) {
  const [peers, setPeers] = useState<PeerCursorInfo[]>([]);

  // Subscribe to presence changes for this cell
  useEffect(() => {
    // Initial fetch
    setPeers(getPeersForCell(cellId));

    // Subscribe to updates
    const unsubscribe = subscribeToCell(cellId, () => {
      setPeers(getPeersForCell(cellId));
    });

    return unsubscribe;
  }, [cellId]);

  if (peers.length === 0) {
    return null;
  }

  const visiblePeers = peers.slice(0, MAX_VISIBLE);
  const overflowCount = peers.length - MAX_VISIBLE;

  return (
    <div className="flex flex-col items-center gap-0.5" title={formatTooltip(peers)}>
      {visiblePeers.map((peer) => (
        <PresenceDot key={peer.peerId} peer={peer} />
      ))}
      {overflowCount > 0 && (
        <span className="text-[9px] font-medium text-muted-foreground leading-none">
          +{overflowCount}
        </span>
      )}
    </div>
  );
}

interface PresenceDotProps {
  peer: PeerCursorInfo;
}

function PresenceDot({ peer }: PresenceDotProps) {
  return (
    <div
      className="w-2 h-2 rounded-full shrink-0 transition-colors"
      style={{ backgroundColor: peer.color }}
      title={peer.peerLabel || "Peer"}
    />
  );
}

function formatTooltip(peers: PeerCursorInfo[]): string {
  if (peers.length === 0) return "";
  if (peers.length === 1) {
    return peers[0].peerLabel || "1 peer";
  }
  const labels = peers
    .map((p) => p.peerLabel)
    .filter(Boolean)
    .join(", ");
  if (labels) {
    return labels;
  }
  return `${peers.length} peers`;
}
