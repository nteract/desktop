import L from "leaflet";
import type { WidgetModel } from "../widget-store";

export function createLeafletIcon(model: WidgetModel): L.Icon | L.DivIcon | undefined {
  const { state, modelName } = model;

  if (modelName === "LeafletIconModel") {
    const iconUrl = state.icon_url as string;
    if (!iconUrl) return undefined;
    return L.icon({
      iconUrl,
      shadowUrl: (state.shadow_url as string) || undefined,
      iconSize: toPoint(state.icon_size),
      shadowSize: toPoint(state.shadow_size),
      iconAnchor: toPoint(state.icon_anchor),
      shadowAnchor: toPoint(state.shadow_anchor),
      popupAnchor: toPoint(state.popup_anchor),
    });
  }

  if (modelName === "LeafletDivIconModel") {
    return L.divIcon({
      html: (state.html as string) || "",
      iconSize: toPoint(state.icon_size) ?? [12, 12],
      iconAnchor: toPoint(state.icon_anchor),
      popupAnchor: toPoint(state.popup_anchor),
      bgPos: toPoint(state.bg_pos),
      className: "leaflet-div-icon",
    });
  }

  if (modelName === "LeafletAwesomeIconModel") {
    const name = (state.name as string) || "home";
    const markerColor = (state.marker_color as string) || "blue";
    const iconColor = (state.icon_color as string) || "white";
    const spin = state.spin as boolean;
    const spinClass = spin ? "fa-spin" : "";
    return L.divIcon({
      html: `<i class="fa fa-${name} ${spinClass}" style="color:${iconColor}"></i>`,
      iconSize: [35, 45],
      iconAnchor: [17, 42],
      popupAnchor: [1, -32],
      className: `awesome-marker awesome-marker-icon-${markerColor}`,
    });
  }

  return undefined;
}

function toPoint(val: unknown): L.PointExpression | undefined {
  if (!Array.isArray(val) || val.length !== 2) return undefined;
  return [val[0] as number, val[1] as number];
}
