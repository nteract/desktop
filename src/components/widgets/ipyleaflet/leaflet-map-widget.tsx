import L from "leaflet";
import "leaflet/dist/leaflet.css";
import { useEffect, useMemo, useRef, useState } from "react";
import type { WidgetComponentProps } from "../widget-registry";
import {
  parseModelRef,
  useWidgetModelValue,
  useWidgetStoreRequired,
} from "../widget-store-context";
import { WidgetView } from "../widget-view";
import { LeafletMapContext, type LeafletMapContextValue } from "./leaflet-map-context";
import { fixDefaultIcon } from "./leaflet-utils";

fixDefaultIcon();

export function LeafletMapWidget({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate, sendCustom } = useWidgetStoreRequired();
  const containerRef = useRef<HTMLDivElement>(null);
  const mapRef = useRef<L.Map | null>(null);
  const localUpdateRef = useRef(false);
  const [mapReady, setMapReady] = useState(false);

  const center = useWidgetModelValue<number[]>(modelId, "center") ?? [0, 0];
  const zoom = useWidgetModelValue<number>(modelId, "zoom") ?? 4;
  const maxZoom = useWidgetModelValue<number | null>(modelId, "max_zoom");
  const minZoom = useWidgetModelValue<number | null>(modelId, "min_zoom");
  const scrollWheelZoom = useWidgetModelValue<boolean>(modelId, "scroll_wheel_zoom") ?? false;
  const dragging = useWidgetModelValue<boolean>(modelId, "dragging") ?? true;
  const touchZoom = useWidgetModelValue<boolean>(modelId, "touch_zoom") ?? true;
  const doubleClickZoom = useWidgetModelValue<boolean>(modelId, "double_click_zoom") ?? true;
  const boxZoom = useWidgetModelValue<boolean>(modelId, "box_zoom") ?? true;
  const keyboard = useWidgetModelValue<boolean>(modelId, "keyboard") ?? true;
  const inertia = useWidgetModelValue<boolean>(modelId, "inertia") ?? true;
  const layers = useWidgetModelValue<string[]>(modelId, "layers") ?? [];
  const controls = useWidgetModelValue<string[]>(modelId, "controls") ?? [];

  useEffect(() => {
    if (!containerRef.current) return;

    const map = L.map(containerRef.current, {
      center: center as L.LatLngExpression,
      zoom,
      maxZoom: maxZoom ?? undefined,
      minZoom: minZoom ?? undefined,
      scrollWheelZoom,
      dragging,
      touchZoom,
      doubleClickZoom,
      boxZoom,
      keyboard,
      inertia,
      zoomControl: false,
      attributionControl: false,
      zoomAnimation: true,
    });

    mapRef.current = map;

    const syncBounds = () => {
      localUpdateRef.current = true;
      const c = map.getCenter();
      const b = map.getBounds();
      sendUpdate(modelId, {
        center: [c.lat, c.lng],
        zoom: map.getZoom(),
        south: b.getSouth(),
        north: b.getNorth(),
        east: b.getEast(),
        west: b.getWest(),
      });
      setTimeout(() => {
        localUpdateRef.current = false;
      }, 200);
    };

    map.on("moveend", syncBounds);
    map.on("zoomend", syncBounds);

    for (const eventType of [
      "click",
      "dblclick",
      "mousedown",
      "mouseup",
      "mouseover",
      "mouseout",
      "mousemove",
      "contextmenu",
      "preclick",
    ] as const) {
      map.on(eventType, (e: L.LeafletMouseEvent) => {
        sendCustom(modelId, {
          event: "interaction",
          type: eventType,
          coordinates: [e.latlng.lat, e.latlng.lng],
        });
      });
    }

    const resizeObserver = new ResizeObserver(() => map.invalidateSize());
    resizeObserver.observe(containerRef.current);

    setMapReady(true);

    return () => {
      resizeObserver.disconnect();
      map.remove();
      mapRef.current = null;
      setMapReady(false);
    };
  }, []);

  // Sync center/zoom from kernel
  useEffect(() => {
    if (!mapRef.current || localUpdateRef.current) return;
    mapRef.current.setView(center as L.LatLngExpression, zoom, { animate: true });
  }, [center, zoom]);

  // Sync interaction options
  useEffect(() => {
    const map = mapRef.current;
    if (!map) return;
    scrollWheelZoom ? map.scrollWheelZoom.enable() : map.scrollWheelZoom.disable();
    dragging ? map.dragging.enable() : map.dragging.disable();
    touchZoom ? map.touchZoom.enable() : map.touchZoom.disable();
    doubleClickZoom ? map.doubleClickZoom.enable() : map.doubleClickZoom.disable();
    boxZoom ? map.boxZoom.enable() : map.boxZoom.disable();
    keyboard ? map.keyboard.enable() : map.keyboard.disable();
  }, [scrollWheelZoom, dragging, touchZoom, doubleClickZoom, boxZoom, keyboard]);

  useEffect(() => {
    if (!mapRef.current) return;
    if (maxZoom != null) mapRef.current.setMaxZoom(maxZoom);
    if (minZoom != null) mapRef.current.setMinZoom(minZoom);
  }, [maxZoom, minZoom]);

  const ctxValue = useMemo<LeafletMapContextValue | null>(
    () => (mapRef.current ? { map: mapRef.current } : null),
    [mapReady],
  );

  const layerIds = layers.map((ref) => parseModelRef(ref)).filter(Boolean) as string[];
  const controlIds = controls.map((ref) => parseModelRef(ref)).filter(Boolean) as string[];

  return (
    <div
      data-widget-id={modelId}
      data-widget-type="LeafletMap"
      className={className}
      style={{ minHeight: "400px", width: "100%", position: "relative" }}
    >
      <div ref={containerRef} style={{ height: "100%", minHeight: "400px", width: "100%" }} />
      {ctxValue && (
        <LeafletMapContext.Provider value={ctxValue}>
          {layerIds.map((id) => (
            <WidgetView key={id} modelId={id} />
          ))}
          {controlIds.map((id) => (
            <WidgetView key={id} modelId={id} />
          ))}
        </LeafletMapContext.Provider>
      )}
    </div>
  );
}
