import { useEffect, useRef } from "react";
import { cn } from "@/lib/utils";

interface GeoJsonOutputProps {
  data: Record<string, unknown>;
  className?: string;
}

/**
 * Render a GeoJSON map inside an isolated iframe using Leaflet.
 *
 * This component expects `window.L` (Leaflet) to be available -- it is
 * injected by the parent app via the iframe library loader before the
 * render message is sent.
 */
export function GeoJsonOutput({ data, className }: GeoJsonOutputProps) {
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    // biome-ignore lint/suspicious/noExplicitAny: Leaflet is injected as a global
    const L = (window as any).L;
    if (!containerRef.current || !data || !L) return;

    const el = containerRef.current;
    const isDark = document.documentElement.classList.contains("dark");

    // Create the map
    const map = L.map(el, {
      zoomAnimation: true,
    });

    // Tile layer -- CartoDB tiles work without an API key
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
    const geojsonLayer = L.geoJSON(data, {
      style: {
        color: featureColor,
        weight: 2,
        fillOpacity: 0.25,
        fillColor: featureColor,
      },
      pointToLayer: (
        _feature: unknown,
        latlng: { lat: number; lng: number },
      ) => {
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
      // Fallback: show the whole world
      map.setView([0, 0], 2);
    }

    // Handle resize so the map tiles render correctly
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
      map.eachLayer((layer: { _url?: string; remove: () => void }) => {
        if (layer._url) layer.remove();
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
      className={cn("not-prose py-2 max-w-full", className)}
      style={{ height: "400px", width: "100%" }}
    />
  );
}
