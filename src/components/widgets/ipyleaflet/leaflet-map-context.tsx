import { createContext, useContext } from "react";
import type L from "leaflet";

export interface LeafletMapContextValue {
  map: L.Map;
}

export const LeafletMapContext = createContext<LeafletMapContextValue | null>(null);

export function useLeafletMap(): LeafletMapContextValue {
  const ctx = useContext(LeafletMapContext);
  if (!ctx) {
    throw new Error("useLeafletMap must be used inside a LeafletMapWidget");
  }
  return ctx;
}
