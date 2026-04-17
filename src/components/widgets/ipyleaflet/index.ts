import { registerWidget } from "../widget-registry";
import {
  LeafletAttributionControlWidget,
  LeafletFullScreenControlWidget,
  LeafletLayersControlWidget,
  LeafletScaleControlWidget,
  LeafletZoomControlWidget,
} from "./leaflet-controls";
import { LeafletGeoJSONWidget } from "./leaflet-geojson";
import { LeafletMapWidget } from "./leaflet-map-widget";
import { LeafletMarkerWidget } from "./leaflet-marker";
import { LeafletTileLayerWidget } from "./leaflet-tile-layer";

// Phase 1: Map + TileLayer + Marker + GeoJSON
registerWidget("LeafletMapModel", LeafletMapWidget);
registerWidget("LeafletTileLayerModel", LeafletTileLayerWidget);
registerWidget("LeafletMarkerModel", LeafletMarkerWidget);
registerWidget("LeafletGeoJSONModel", LeafletGeoJSONWidget);

// Headless models — state consumed by parent widgets, no visual render
const NullWidget = () => null;
registerWidget("LeafletIconModel", NullWidget);
registerWidget("LeafletAwesomeIconModel", NullWidget);
registerWidget("LeafletDivIconModel", NullWidget);
registerWidget("LeafletMapStyleModel", NullWidget);

// Stub registrations for models we don't render yet — prevents "Unsupported widget" fallback
registerWidget("LeafletLayerModel", NullWidget);
registerWidget("LeafletUILayerModel", NullWidget);
registerWidget("LeafletRasterLayerModel", NullWidget);
registerWidget("LeafletVectorLayerModel", NullWidget);
registerWidget("LeafletPathModel", NullWidget);
registerWidget("LeafletPolylineModel", NullWidget);
registerWidget("LeafletPolygonModel", NullWidget);
registerWidget("LeafletRectangleModel", NullWidget);
registerWidget("LeafletCircleMarkerModel", NullWidget);
registerWidget("LeafletCircleModel", NullWidget);
registerWidget("LeafletMarkerClusterModel", NullWidget);
registerWidget("LeafletLayerGroupModel", NullWidget);
registerWidget("LeafletFeatureGroupModel", NullWidget);
registerWidget("LeafletHeatmapModel", NullWidget);
registerWidget("LeafletImageOverlayModel", NullWidget);
registerWidget("LeafletVideoOverlayModel", NullWidget);
registerWidget("LeafletWMSLayerModel", NullWidget);
registerWidget("LeafletPopupModel", NullWidget);
registerWidget("LeafletControlModel", NullWidget);
registerWidget("LeafletZoomControlModel", LeafletZoomControlWidget);
registerWidget("LeafletScaleControlModel", LeafletScaleControlWidget);
registerWidget("LeafletAttributionControlModel", LeafletAttributionControlWidget);
registerWidget("LeafletFullScreenControlModel", LeafletFullScreenControlWidget);
registerWidget("LeafletLayersControlModel", LeafletLayersControlWidget);
registerWidget("LeafletDrawControlModel", NullWidget);
registerWidget("LeafletGeomanDrawControlModel", NullWidget);
registerWidget("LeafletMeasureControlModel", NullWidget);
registerWidget("LeafletLegendControlModel", NullWidget);
registerWidget("LeafletWidgetControlModel", NullWidget);
registerWidget("LeafletSearchControlModel", NullWidget);
registerWidget("LeafletSplitMapControlModel", NullWidget);
registerWidget("LeafletLocalTileLayerModel", NullWidget);
registerWidget("LeafletVectorTileLayerModel", NullWidget);
registerWidget("LeafletPMTilesLayerModel", NullWidget);
registerWidget("LeafletImageServiceModel", NullWidget);
registerWidget("LeafletAntPathModel", NullWidget);
registerWidget("LeafletMagnifyingGlassModel", NullWidget);
