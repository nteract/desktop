/**
 * React context for presence functionality.
 *
 * Provides cursor/selection sending methods to the component tree,
 * wrapping the usePresence hook to avoid prop drilling.
 */

import { createContext, type ReactNode, useCallback, useContext, useMemo } from "react";
import { usePresence } from "../hooks/usePresence";

export interface PresenceContextValue {
  /** Send cursor position for a cell */
  setCursor: (cellId: string, line: number, column: number) => void;
  /** Send selection range for a cell */
  setSelection: (
    cellId: string,
    anchorLine: number,
    anchorCol: number,
    headLine: number,
    headCol: number,
  ) => void;
  /** Send cell-level focus (no cursor position) */
  setFocus: (cellId: string) => void;
  /** The local peer's ID */
  peerId: string | null;
}

const PresenceContext = createContext<PresenceContextValue | null>(null);

interface PresenceProviderProps {
  peerId: string;
  peerLabel?: string;
  actorLabel?: string;
  children: ReactNode;
}

export function PresenceProvider({
  peerId,
  peerLabel = "",
  actorLabel = "",
  children,
}: PresenceProviderProps) {
  const presence = usePresence(peerId, peerLabel, actorLabel);

  const setCursor = useCallback(
    (cellId: string, line: number, column: number) => {
      presence.setCursor(cellId, line, column);
    },
    [presence],
  );

  const setSelection = useCallback(
    (cellId: string, anchorLine: number, anchorCol: number, headLine: number, headCol: number) => {
      presence.setSelection(cellId, anchorLine, anchorCol, headLine, headCol);
    },
    [presence],
  );

  const setFocus = useCallback(
    (cellId: string) => {
      presence.setFocus(cellId);
    },
    [presence],
  );

  const value = useMemo<PresenceContextValue>(
    () => ({
      setCursor,
      setSelection,
      setFocus,
      peerId,
    }),
    [setCursor, setSelection, setFocus, peerId],
  );

  return <PresenceContext.Provider value={value}>{children}</PresenceContext.Provider>;
}

/**
 * Hook to access presence context.
 * Returns null if used outside a PresenceProvider.
 */
export function usePresenceContext(): PresenceContextValue | null {
  return useContext(PresenceContext);
}

/**
 * Hook that throws if presence context is not available.
 * Use this when you need presence and it's an error if it's missing.
 */
export function usePresenceContextRequired(): PresenceContextValue {
  const ctx = useContext(PresenceContext);
  if (!ctx) {
    throw new Error("usePresenceContextRequired must be used within a PresenceProvider");
  }
  return ctx;
}
