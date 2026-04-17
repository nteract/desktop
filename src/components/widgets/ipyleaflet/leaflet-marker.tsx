import L from "leaflet";
import { useEffect, useRef } from "react";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";
import { createLeafletIcon } from "./leaflet-icon";
import { useLeafletMap } from "./leaflet-map-context";

export function LeafletMarkerWidget({ modelId }: WidgetComponentProps) {
  const { map } = useLeafletMap();
  const { sendUpdate, sendCustom, store } = useWidgetStoreRequired();
  const markerRef = useRef<L.Marker | null>(null);
  const localMoveRef = useRef(false);

  const location = useWidgetModelValue<number[]>(modelId, "location") ?? [0, 0];
  const opacity = useWidgetModelValue<number>(modelId, "opacity") ?? 1.0;
  const visible = useWidgetModelValue<boolean>(modelId, "visible") ?? true;
  const draggable = useWidgetModelValue<boolean>(modelId, "draggable") ?? true;
  const title = useWidgetModelValue<string>(modelId, "title") ?? "";
  const alt = useWidgetModelValue<string>(modelId, "alt") ?? "";
  const rotationAngle = useWidgetModelValue<number>(modelId, "rotation_angle") ?? 0;
  const iconRef = useWidgetModelValue<string>(modelId, "icon");
  const zIndexOffset = useWidgetModelValue<number>(modelId, "z_index_offset") ?? 0;

  useEffect(() => {
    let icon: L.Icon | L.DivIcon | undefined;
    if (iconRef && typeof iconRef === "string") {
      const refMatch = iconRef.match(/^IPY_MODEL_(.+)$/);
      if (refMatch) {
        const iconModel = store.getModel(refMatch[1]);
        if (iconModel) icon = createLeafletIcon(iconModel);
      }
    }

    const marker = L.marker(location as L.LatLngExpression, {
      draggable,
      title,
      alt,
      opacity: visible ? opacity : 0,
      zIndexOffset,
      icon: icon ?? new L.Icon.Default(),
    });

    if (rotationAngle !== 0 && "setRotationAngle" in marker) {
      (marker as unknown as { setRotationAngle: (a: number) => void }).setRotationAngle(
        rotationAngle,
      );
    }

    marker.addTo(map);

    marker.on("dragend", () => {
      const pos = marker.getLatLng();
      localMoveRef.current = true;
      sendUpdate(modelId, { location: [pos.lat, pos.lng] });
      sendCustom(modelId, { event: "move", location: [pos.lat, pos.lng] });
      setTimeout(() => {
        localMoveRef.current = false;
      }, 100);
    });

    marker.on("click", (e) => {
      sendCustom(modelId, { type: "click", coordinates: [e.latlng.lat, e.latlng.lng] });
    });
    marker.on("dblclick", (e) => {
      sendCustom(modelId, { type: "dblclick", coordinates: [e.latlng.lat, e.latlng.lng] });
    });

    markerRef.current = marker;
    return () => {
      marker.remove();
      markerRef.current = null;
    };
  }, [map, draggable, title, alt, iconRef, zIndexOffset]);

  useEffect(() => {
    if (!markerRef.current || localMoveRef.current) return;
    markerRef.current.setLatLng(location as L.LatLngExpression);
  }, [location]);

  useEffect(() => {
    markerRef.current?.setOpacity(visible ? opacity : 0);
  }, [opacity, visible]);

  return null;
}
