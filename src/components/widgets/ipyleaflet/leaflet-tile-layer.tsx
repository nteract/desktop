import L from "leaflet";
import { useEffect, useRef } from "react";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue } from "../widget-store-context";
import { useLeafletMap } from "./leaflet-map-context";

export function LeafletTileLayerWidget({ modelId }: WidgetComponentProps) {
  const { map } = useLeafletMap();
  const layerRef = useRef<L.TileLayer | null>(null);

  const url =
    useWidgetModelValue<string>(modelId, "url") ?? "https://tile.openstreetmap.org/{z}/{x}/{y}.png";
  const minZoom = useWidgetModelValue<number>(modelId, "min_zoom") ?? 0;
  const maxZoom = useWidgetModelValue<number>(modelId, "max_zoom") ?? 18;
  const attribution = useWidgetModelValue<string>(modelId, "attribution");
  const opacity = useWidgetModelValue<number>(modelId, "opacity") ?? 1.0;
  const visible = useWidgetModelValue<boolean>(modelId, "visible") ?? true;
  const tileSize = useWidgetModelValue<number>(modelId, "tile_size") ?? 256;
  const noWrap = useWidgetModelValue<boolean>(modelId, "no_wrap") ?? false;
  const tms = useWidgetModelValue<boolean>(modelId, "tms") ?? false;
  const detectRetina = useWidgetModelValue<boolean>(modelId, "detect_retina") ?? false;

  useEffect(() => {
    const layer = L.tileLayer(url, {
      minZoom,
      maxZoom,
      attribution: attribution ?? undefined,
      opacity: visible ? opacity : 0,
      tileSize,
      noWrap,
      tms,
      detectRetina,
    });
    layer.addTo(map);
    layerRef.current = layer;

    return () => {
      layer.remove();
      layerRef.current = null;
    };
  }, [map, url, minZoom, maxZoom, attribution, tileSize, noWrap, tms, detectRetina]);

  useEffect(() => {
    layerRef.current?.setOpacity(visible ? opacity : 0);
  }, [opacity, visible]);

  return null;
}
