/**
 * Leaflet/GeoJSON Renderer Plugin
 *
 * On-demand renderer plugin for application/geo+json outputs.
 * Bundles Leaflet directly — no window.L global.
 * Loaded into the isolated iframe via the renderer plugin API.
 *
 * Leaflet CSS is delivered via the plugin's css channel and injected
 * as a <style> tag by the iframe's installRendererPlugin() handler.
 */

import L from "leaflet";
import "leaflet/dist/leaflet.css";
import { useEffect, useRef } from "react";
import { cn } from "@/lib/utils";

// --- Types ---

interface RendererProps {
  data: unknown;
  metadata?: Record<string, unknown>;
  mimeType: string;
}

// --- GeoJSON Renderer ---

function GeoJsonRenderer({ data: rawData }: RendererProps) {
  const containerRef = useRef<HTMLDivElement>(null);

  const data =
    typeof rawData === "string"
      ? (JSON.parse(rawData) as Record<string, unknown>)
      : (rawData as Record<string, unknown>);

  useEffect(() => {
    if (!containerRef.current || !data) return;

    const el = containerRef.current;
    const isDark = document.documentElement.classList.contains("dark");

    // Create the map
    const map = L.map(el, { zoomAnimation: true });

    // Tile layer — CartoDB tiles work without an API key
    const tileUrl = isDark
      ? "https://{s}.basemaps.cartocdn.com/dark_all/{z}/{x}/{y}{r}.png"
      : "https://{s}.basemaps.cartocdn.com/light_all/{z}/{x}/{y}{r}.png";

    L.tileLayer(tileUrl, {
      attribution:
        '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> &copy; <a href="https://carto.com/">CARTO</a>',
      maxZoom: 19,
    }).addTo(map);

    // Style features
    const featureColor = isDark ? "#818cf8" : "#4f46e5";
    const geojsonLayer = L.geoJSON(data as GeoJSON.GeoJsonObject, {
      style: {
        color: featureColor,
        weight: 2,
        fillOpacity: 0.25,
        fillColor: featureColor,
      },
      pointToLayer: (_feature, latlng) => {
        return L.circleMarker(latlng, {
          radius: 6,
          color: featureColor,
          weight: 2,
          fillOpacity: 0.5,
          fillColor: featureColor,
        });
      },
    }).addTo(map);

    // Fit to bounds of the GeoJSON features
    const bounds = geojsonLayer.getBounds();
    if (bounds.isValid()) {
      map.fitBounds(bounds, { padding: [20, 20] });
    } else {
      map.setView([0, 0], 2);
    }

    // Handle resize so map tiles render correctly
    const resizeObserver = new ResizeObserver(() => {
      map.invalidateSize();
    });
    resizeObserver.observe(el);

    // React to theme changes
    const themeObserver = new MutationObserver(() => {
      const nowDark = document.documentElement.classList.contains("dark");
      const newTileUrl = nowDark
        ? "https://{s}.basemaps.cartocdn.com/dark_all/{z}/{x}/{y}{r}.png"
        : "https://{s}.basemaps.cartocdn.com/light_all/{z}/{x}/{y}{r}.png";
      const newColor = nowDark ? "#818cf8" : "#4f46e5";

      // Replace tile layer
      map.eachLayer((layer) => {
        if ((layer as L.TileLayer).getTileUrl) layer.remove();
      });
      L.tileLayer(newTileUrl, {
        attribution:
          '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> &copy; <a href="https://carto.com/">CARTO</a>',
        maxZoom: 19,
      }).addTo(map);

      // Update GeoJSON layer style
      geojsonLayer.setStyle({
        color: newColor,
        fillColor: newColor,
      });
    });
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["class"],
    });

    return () => {
      themeObserver.disconnect();
      resizeObserver.disconnect();
      map.remove();
    };
  }, [data]);

  if (!data) return null;

  return (
    <div
      ref={containerRef}
      data-slot="geojson-output"
      className={cn("not-prose py-2 max-w-full")}
      style={{ height: "400px", width: "100%" }}
    />
  );
}

// --- Plugin install ---

export function install(ctx: {
  register: (
    mimeTypes: string[],
    component: React.ComponentType<RendererProps>,
  ) => void;
}) {
  ctx.register(["application/geo+json"], GeoJsonRenderer);
}
