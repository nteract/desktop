import L from "leaflet";
import { useEffect, useRef } from "react";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue } from "../widget-store-context";
import { useLeafletMap } from "./leaflet-map-context";

type Position = "topleft" | "topright" | "bottomleft" | "bottomright";

function useControlPosition(modelId: string): Position {
  return (useWidgetModelValue<string>(modelId, "position") as Position) ?? "topleft";
}

export function LeafletZoomControlWidget({ modelId }: WidgetComponentProps) {
  const { map } = useLeafletMap();
  const controlRef = useRef<L.Control.Zoom | null>(null);
  const position = useControlPosition(modelId);
  const zoomInText = useWidgetModelValue<string>(modelId, "zoom_in_text") ?? "+";
  const zoomInTitle = useWidgetModelValue<string>(modelId, "zoom_in_title") ?? "Zoom in";
  const zoomOutText = useWidgetModelValue<string>(modelId, "zoom_out_text") ?? "-";
  const zoomOutTitle = useWidgetModelValue<string>(modelId, "zoom_out_title") ?? "Zoom out";

  useEffect(() => {
    const control = L.control.zoom({
      position,
      zoomInText,
      zoomInTitle,
      zoomOutText,
      zoomOutTitle,
    });
    control.addTo(map);
    controlRef.current = control;
    return () => {
      control.remove();
      controlRef.current = null;
    };
  }, [map, position, zoomInText, zoomInTitle, zoomOutText, zoomOutTitle]);

  return null;
}

export function LeafletAttributionControlWidget({ modelId }: WidgetComponentProps) {
  const { map } = useLeafletMap();
  const controlRef = useRef<L.Control.Attribution | null>(null);
  const position = useControlPosition(modelId);
  const prefix = useWidgetModelValue<string>(modelId, "prefix") ?? "ipyleaflet";

  useEffect(() => {
    const control = L.control.attribution({ position, prefix });
    control.addTo(map);
    controlRef.current = control;
    return () => {
      control.remove();
      controlRef.current = null;
    };
  }, [map, position, prefix]);

  return null;
}

export function LeafletScaleControlWidget({ modelId }: WidgetComponentProps) {
  const { map } = useLeafletMap();
  const controlRef = useRef<L.Control.Scale | null>(null);
  const position = useControlPosition(modelId);
  const maxWidth = useWidgetModelValue<number>(modelId, "max_width") ?? 100;
  const metric = useWidgetModelValue<boolean>(modelId, "metric") ?? true;
  const imperial = useWidgetModelValue<boolean>(modelId, "imperial") ?? true;

  useEffect(() => {
    const control = L.control.scale({ position, maxWidth, metric, imperial });
    control.addTo(map);
    controlRef.current = control;
    return () => {
      control.remove();
      controlRef.current = null;
    };
  }, [map, position, maxWidth, metric, imperial]);

  return null;
}

export function LeafletLayersControlWidget({ modelId }: WidgetComponentProps) {
  const { map } = useLeafletMap();
  const controlRef = useRef<L.Control.Layers | null>(null);
  const position = useControlPosition(modelId);
  const collapsed = useWidgetModelValue<boolean>(modelId, "collapsed") ?? true;

  useEffect(() => {
    const control = L.control.layers({}, {}, { position, collapsed });
    control.addTo(map);
    controlRef.current = control;
    return () => {
      control.remove();
      controlRef.current = null;
    };
  }, [map, position, collapsed]);

  return null;
}

export function LeafletFullScreenControlWidget({ modelId }: WidgetComponentProps) {
  const { map } = useLeafletMap();
  const position = useControlPosition(modelId);

  useEffect(() => {
    const container = L.DomUtil.create("div", "leaflet-bar leaflet-control");
    const button = L.DomUtil.create("a", "", container);
    button.innerHTML = "&#x26F6;";
    button.title = "Full Screen";
    button.href = "#";
    button.role = "button";
    button.setAttribute("aria-label", "Full Screen");

    L.DomEvent.disableClickPropagation(container);
    L.DomEvent.on(button, "click", (e) => {
      L.DomEvent.preventDefault(e);
      const el = map.getContainer();
      if (document.fullscreenElement) {
        document.exitFullscreen();
      } else {
        el.requestFullscreen();
      }
    });

    const Control = L.Control.extend({
      onAdd: () => container,
      onRemove: () => {},
    });
    const control = new Control({ position }) as L.Control;
    control.addTo(map);

    return () => {
      control.remove();
    };
  }, [map, position]);

  return null;
}
