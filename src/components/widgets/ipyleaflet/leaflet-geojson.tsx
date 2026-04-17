import L from "leaflet";
import { useEffect, useRef } from "react";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";
import { useLeafletMap } from "./leaflet-map-context";

export function LeafletGeoJSONWidget({ modelId }: WidgetComponentProps) {
  const { map } = useLeafletMap();
  const { sendCustom } = useWidgetStoreRequired();
  const layerRef = useRef<L.GeoJSON | null>(null);

  const data = useWidgetModelValue<Record<string, unknown>>(modelId, "data") ?? {};
  const style = useWidgetModelValue<Record<string, unknown>>(modelId, "style") ?? {};
  const hoverStyle = useWidgetModelValue<Record<string, unknown>>(modelId, "hover_style") ?? {};
  const pointStyle = useWidgetModelValue<Record<string, unknown>>(modelId, "point_style") ?? {};
  const visible = useWidgetModelValue<boolean>(modelId, "visible") ?? true;

  useEffect(() => {
    if (!data || !data.type) return;

    const hasHover = Object.keys(hoverStyle).length > 0;

    const layer = L.geoJSON(data as GeoJSON.GeoJsonObject, {
      style: Object.keys(style).length > 0 ? (style as L.PathOptions) : undefined,
      pointToLayer: (_feature, latlng) => {
        if (Object.keys(pointStyle).length > 0) {
          return L.circleMarker(latlng, pointStyle as L.CircleMarkerOptions);
        }
        return L.circleMarker(latlng, { radius: 6, ...(style as L.CircleMarkerOptions) });
      },
      onEachFeature: (feature, featureLayer) => {
        featureLayer.on("click", (e) => {
          sendCustom(modelId, {
            event: "click",
            feature: feature.properties,
            id: feature.id,
            coordinates: [e.latlng.lat, e.latlng.lng],
          });
        });
        featureLayer.on("mouseover", (e) => {
          sendCustom(modelId, {
            event: "mouseover",
            feature: feature.properties,
            id: feature.id,
            coordinates: [e.latlng.lat, e.latlng.lng],
          });
          if (hasHover && "setStyle" in featureLayer) {
            (featureLayer as L.Path).setStyle(hoverStyle as L.PathOptions);
          }
        });
        featureLayer.on("mouseout", () => {
          sendCustom(modelId, {
            event: "mouseout",
            feature: feature.properties,
            id: feature.id,
          });
          if (hasHover) {
            layer.resetStyle(featureLayer as L.Path);
          }
        });
      },
    });

    if (visible) layer.addTo(map);
    layerRef.current = layer;

    return () => {
      layer.remove();
      layerRef.current = null;
    };
  }, [map, data, style, hoverStyle, pointStyle]);

  useEffect(() => {
    const layer = layerRef.current;
    if (!layer) return;
    if (visible) {
      if (!map.hasLayer(layer)) layer.addTo(map);
    } else {
      layer.remove();
    }
  }, [visible, map]);

  return null;
}
