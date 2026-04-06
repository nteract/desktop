//! GeoJSON summarization.
//!
//! Walks a GeoJSON value (FeatureCollection, Feature, or bare Geometry) and
//! produces a compact text summary suitable for LLMs — feature count, geometry
//! types, property schema, and bounding box.

use std::collections::HashSet;

use serde_json::Value;

use crate::stats::fmt_num;

/// Summarize a GeoJSON value into an LLM-friendly text representation.
pub fn summarize(spec: &Value) -> String {
    let geo_type = spec
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown");

    match geo_type {
        "FeatureCollection" => summarize_feature_collection(spec),
        "Feature" => summarize_feature(spec),
        // Bare geometry object (Point, LineString, Polygon, etc.)
        _ => summarize_geometry_object(spec),
    }
}

/// Summarize a FeatureCollection.
fn summarize_feature_collection(spec: &Value) -> String {
    let mut lines = Vec::new();

    let features = spec.get("features").and_then(|v| v.as_array());
    let count = features.map(|f| f.len()).unwrap_or(0);

    // Header
    lines.push(format!("GeoJSON FeatureCollection: {count} feature(s)"));

    if let Some(features) = features {
        // Geometry types
        let geom_types = collect_geometry_types(features);
        if !geom_types.is_empty() {
            lines.push(format!("Geometry types: [{}]", geom_types.join(", ")));
        }

        // Property schema from first feature
        if let Some(schema) = property_schema(features) {
            lines.push(format!("Properties: [{schema}]"));
        }

        // Bounding box: use explicit `bbox` field or compute from coordinates
        let bbox_line = spec
            .get("bbox")
            .and_then(format_bbox)
            .map(|b| format!("Bbox: {b}"))
            .or_else(|| compute_bbox(features));
        if let Some(b) = bbox_line {
            lines.push(b);
        }
    }

    lines.join("\n")
}

/// Summarize a single Feature.
fn summarize_feature(spec: &Value) -> String {
    let mut lines = Vec::new();

    let geom_type = spec
        .get("geometry")
        .and_then(|g| g.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown");

    // Try to find a human-readable label: check top-level `id` (per GeoJSON
    // spec), then common property fields. Handles both string and numeric ids.
    let label = spec.get("id").and_then(value_as_label).or_else(|| {
        let props = spec.get("properties")?;
        for key in &["name", "NAME", "title", "id", "ID"] {
            if let Some(s) = props.get(*key).and_then(value_as_label) {
                return Some(s);
            }
        }
        None
    });

    match label {
        Some(name) => lines.push(format!("GeoJSON Feature ({geom_type}): \"{name}\"")),
        None => lines.push(format!("GeoJSON Feature ({geom_type})")),
    }

    // Property keys
    if let Some(props) = spec.get("properties").and_then(|v| v.as_object()) {
        if !props.is_empty() {
            let keys: Vec<&str> = props.keys().map(|k| k.as_str()).collect();
            let display = truncate_list(&keys, 10);
            lines.push(format!("Properties: [{display}]"));
        }
    }

    // Bbox from explicit field or geometry coordinates
    let bbox_line = spec
        .get("bbox")
        .and_then(format_bbox)
        .map(|b| format!("Bbox: {b}"))
        .or_else(|| {
            spec.get("geometry")
                .and_then(bbox_from_geometry)
                .map(|b| format!("Bbox: {}", format_bbox_values(b.0, b.1, b.2, b.3)))
        });
    if let Some(b) = bbox_line {
        lines.push(b);
    }

    lines.join("\n")
}

/// Summarize a bare geometry object.
fn summarize_geometry_object(spec: &Value) -> String {
    let geo_type = spec
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown");

    match geo_type {
        "GeometryCollection" => {
            let geometries = spec.get("geometries").and_then(|v| v.as_array());
            let count = geometries.map(|g| g.len()).unwrap_or(0);
            let types: Vec<String> = geometries
                .map(|gs| {
                    let mut seen_set = HashSet::new();
                    let mut seen = Vec::new();
                    for g in gs {
                        if let Some(t) = g.get("type").and_then(|v| v.as_str()) {
                            if seen_set.insert(t) {
                                seen.push(t.to_string());
                            }
                        }
                    }
                    seen
                })
                .unwrap_or_default();

            if types.is_empty() {
                format!("GeoJSON GeometryCollection: {count} geometries")
            } else {
                format!(
                    "GeoJSON GeometryCollection: {count} geometries [{}]",
                    types.join(", ")
                )
            }
        }
        "Point" => {
            if let Some(coords) = spec.get("coordinates").and_then(|v| v.as_array()) {
                let lon = coords.first().and_then(|v| v.as_f64());
                let lat = coords.get(1).and_then(|v| v.as_f64());
                if let (Some(lon), Some(lat)) = (lon, lat) {
                    return format!("GeoJSON Point (lat={}, lon={})", fmt_num(lat), fmt_num(lon));
                }
            }
            "GeoJSON Point".to_string()
        }
        _ => {
            // LineString, Polygon, Multi* — show type and coordinate count
            let coord_count = count_coordinates(spec);
            if coord_count > 0 {
                format!("GeoJSON {geo_type} ({coord_count} coordinates)")
            } else {
                format!("GeoJSON {geo_type}")
            }
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Extract a display label from a JSON value (string, number, or bool).
fn value_as_label(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Collect deduplicated geometry types from a feature array, preserving
/// insertion order. Uses a HashSet for O(1) membership checks.
fn collect_geometry_types(features: &[Value]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut types = Vec::new();
    for f in features {
        if let Some(t) = f
            .get("geometry")
            .and_then(|g| g.get("type"))
            .and_then(|v| v.as_str())
        {
            if seen.insert(t) {
                types.push(t.to_string());
            }
        }
    }
    types
}

/// Extract property field names from the first feature that has properties.
/// Returns a comma-separated string or None.
fn property_schema(features: &[Value]) -> Option<String> {
    for f in features {
        if let Some(props) = f.get("properties").and_then(|v| v.as_object()) {
            if !props.is_empty() {
                let keys: Vec<&str> = props.keys().map(|k| k.as_str()).collect();
                return Some(truncate_list(&keys, 10));
            }
        }
    }
    None
}

/// Format an explicit GeoJSON `bbox` array: [west, south, east, north].
fn format_bbox(bbox: &Value) -> Option<String> {
    let arr = bbox.as_array()?;
    if arr.len() < 4 {
        return None;
    }
    let west = arr[0].as_f64()?;
    let south = arr[1].as_f64()?;
    let east = arr[2].as_f64()?;
    let north = arr[3].as_f64()?;
    Some(format_bbox_values(west, south, east, north))
}

fn format_bbox_values(west: f64, south: f64, east: f64, north: f64) -> String {
    format!(
        "[{}, {}, {}, {}]",
        fmt_num(west),
        fmt_num(south),
        fmt_num(east),
        fmt_num(north)
    )
}

/// Compute a bounding box from feature geometries by walking coordinates.
/// Returns a formatted "Bbox: …" or "Bbox (approx): …" line, or None.
fn compute_bbox(features: &[Value]) -> Option<String> {
    let mut west = f64::INFINITY;
    let mut south = f64::INFINITY;
    let mut east = f64::NEG_INFINITY;
    let mut north = f64::NEG_INFINITY;
    let mut found = false;

    for f in features {
        if let Some(geom) = f.get("geometry") {
            walk_coordinates(geom, &mut |lon, lat| {
                found = true;
                if lon < west {
                    west = lon;
                }
                if lon > east {
                    east = lon;
                }
                if lat < south {
                    south = lat;
                }
                if lat > north {
                    north = lat;
                }
            });
        }
    }

    if !found {
        return None;
    }

    let values = format_bbox_values(west, south, east, north);
    // Check if coordinate walk was truncated
    let total = features
        .iter()
        .filter_map(|f| f.get("geometry"))
        .map(count_coordinates)
        .sum::<usize>();
    if total >= MAX_WALK_COORDS {
        Some(format!("Bbox (approx): {values}"))
    } else {
        Some(format!("Bbox: {values}"))
    }
}

/// Extract bounding box from a single geometry.
fn bbox_from_geometry(geom: &Value) -> Option<(f64, f64, f64, f64)> {
    let mut west = f64::INFINITY;
    let mut south = f64::INFINITY;
    let mut east = f64::NEG_INFINITY;
    let mut north = f64::NEG_INFINITY;
    let mut found = false;

    walk_coordinates(geom, &mut |lon, lat| {
        found = true;
        if lon < west {
            west = lon;
        }
        if lon > east {
            east = lon;
        }
        if lat < south {
            south = lat;
        }
        if lat > north {
            north = lat;
        }
    });

    if found {
        Some((west, south, east, north))
    } else {
        None
    }
}

/// Maximum coordinate pairs to walk before stopping.
const MAX_WALK_COORDS: usize = 10_000;

/// Walk all [lon, lat, …] coordinate pairs in a geometry, calling `cb` for each.
///
/// Handles Point, MultiPoint, LineString, MultiLineString, Polygon,
/// MultiPolygon, and GeometryCollection. Limits traversal to
/// [`MAX_WALK_COORDS`] pairs to avoid spending too long on huge datasets.
fn walk_coordinates(geom: &Value, cb: &mut dyn FnMut(f64, f64)) {
    let mut count = 0;
    walk_coordinates_inner(geom, cb, &mut count, MAX_WALK_COORDS);
}

fn walk_coordinates_inner(
    geom: &Value,
    cb: &mut dyn FnMut(f64, f64),
    count: &mut usize,
    max: usize,
) {
    if *count >= max {
        return;
    }

    let geo_type = match geom.get("type").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return,
    };

    if geo_type == "GeometryCollection" {
        if let Some(geometries) = geom.get("geometries").and_then(|v| v.as_array()) {
            for g in geometries {
                walk_coordinates_inner(g, cb, count, max);
                if *count >= max {
                    return;
                }
            }
        }
        return;
    }

    let coords = match geom.get("coordinates") {
        Some(c) => c,
        None => return,
    };

    match geo_type {
        "Point" => {
            if let Some(pair) = as_coord_pair(coords) {
                *count += 1;
                cb(pair.0, pair.1);
            }
        }
        "MultiPoint" | "LineString" => {
            if let Some(arr) = coords.as_array() {
                for c in arr {
                    if *count >= max {
                        return;
                    }
                    if let Some(pair) = as_coord_pair(c) {
                        *count += 1;
                        cb(pair.0, pair.1);
                    }
                }
            }
        }
        "MultiLineString" | "Polygon" => {
            if let Some(rings) = coords.as_array() {
                for ring in rings {
                    if let Some(arr) = ring.as_array() {
                        for c in arr {
                            if *count >= max {
                                return;
                            }
                            if let Some(pair) = as_coord_pair(c) {
                                *count += 1;
                                cb(pair.0, pair.1);
                            }
                        }
                    }
                }
            }
        }
        "MultiPolygon" => {
            if let Some(polys) = coords.as_array() {
                for poly in polys {
                    if let Some(rings) = poly.as_array() {
                        for ring in rings {
                            if let Some(arr) = ring.as_array() {
                                for c in arr {
                                    if *count >= max {
                                        return;
                                    }
                                    if let Some(pair) = as_coord_pair(c) {
                                        *count += 1;
                                        cb(pair.0, pair.1);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// Try to parse a JSON value as a [lon, lat] coordinate pair.
fn as_coord_pair(v: &Value) -> Option<(f64, f64)> {
    let arr = v.as_array()?;
    if arr.len() < 2 {
        return None;
    }
    let lon = arr[0].as_f64()?;
    let lat = arr[1].as_f64()?;
    if lon.is_finite() && lat.is_finite() {
        Some((lon, lat))
    } else {
        None
    }
}

/// Count total coordinate positions in a geometry (recursively).
fn count_coordinates(geom: &Value) -> usize {
    let mut count = 0;
    walk_coordinates(geom, &mut |_, _| count += 1);
    count
}

/// Format a list of strings, truncating with "+ N more" if over `max` items.
fn truncate_list(items: &[&str], max: usize) -> String {
    if items.len() <= max {
        items.join(", ")
    } else {
        let shown: Vec<&str> = items[..max].to_vec();
        format!("{}, +{} more", shown.join(", "), items.len() - max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── FeatureCollection ───────────────────────────────────────────

    #[test]
    fn test_feature_collection_basic() {
        let spec = json!({
            "type": "FeatureCollection",
            "features": [
                {
                    "type": "Feature",
                    "geometry": {"type": "Point", "coordinates": [-122.4, 37.8]},
                    "properties": {"name": "San Francisco", "pop": 870000}
                },
                {
                    "type": "Feature",
                    "geometry": {"type": "Point", "coordinates": [-118.2, 34.1]},
                    "properties": {"name": "Los Angeles", "pop": 3900000}
                }
            ]
        });
        let result = summarize(&spec);
        assert!(result.contains("GeoJSON FeatureCollection: 2 feature(s)"));
        assert!(result.contains("Geometry types: [Point]"));
        // Don't depend on map key iteration order
        assert!(result.contains("Properties: ["));
        assert!(result.contains("name"));
        assert!(result.contains("pop"));
        assert!(result.contains("Bbox:"));
    }

    #[test]
    fn test_feature_collection_mixed_types() {
        let spec = json!({
            "type": "FeatureCollection",
            "features": [
                {
                    "type": "Feature",
                    "geometry": {"type": "Point", "coordinates": [0.0, 0.0]},
                    "properties": {"id": 1}
                },
                {
                    "type": "Feature",
                    "geometry": {"type": "Polygon", "coordinates": [[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 0.0]]]},
                    "properties": {"id": 2}
                },
                {
                    "type": "Feature",
                    "geometry": {"type": "LineString", "coordinates": [[0.0, 0.0], [2.0, 2.0]]},
                    "properties": {"id": 3}
                }
            ]
        });
        let result = summarize(&spec);
        assert!(result.contains("3 feature(s)"));
        assert!(result.contains("Geometry types: [Point, Polygon, LineString]"));
    }

    #[test]
    fn test_feature_collection_explicit_bbox() {
        let spec = json!({
            "type": "FeatureCollection",
            "bbox": [-180.0, -90.0, 180.0, 90.0],
            "features": []
        });
        let result = summarize(&spec);
        assert!(result.contains("0 feature(s)"));
        assert!(result.contains("Bbox: [-180, -90, 180, 90]"));
    }

    #[test]
    fn test_feature_collection_empty() {
        let spec = json!({
            "type": "FeatureCollection",
            "features": []
        });
        let result = summarize(&spec);
        assert!(result.contains("0 feature(s)"));
        assert!(!result.contains("Geometry types:"));
        assert!(!result.contains("Properties:"));
    }

    #[test]
    fn test_feature_collection_no_properties() {
        let spec = json!({
            "type": "FeatureCollection",
            "features": [
                {
                    "type": "Feature",
                    "geometry": {"type": "Point", "coordinates": [1.0, 2.0]},
                    "properties": null
                }
            ]
        });
        let result = summarize(&spec);
        assert!(result.contains("1 feature(s)"));
        assert!(!result.contains("Properties:"));
    }

    // ── Single Feature ──────────────────────────────────────────────

    #[test]
    fn test_feature_with_name() {
        let spec = json!({
            "type": "Feature",
            "geometry": {"type": "Polygon", "coordinates": [[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 0.0]]]},
            "properties": {"name": "Central Park", "area_km2": 3.41}
        });
        let result = summarize(&spec);
        assert!(result.contains("GeoJSON Feature (Polygon): \"Central Park\""));
        // Don't depend on map key iteration order
        assert!(result.contains("Properties: ["));
        assert!(result.contains("area_km2"));
        assert!(result.contains("name"));
    }

    #[test]
    fn test_feature_without_name() {
        let spec = json!({
            "type": "Feature",
            "geometry": {"type": "LineString", "coordinates": [[0.0, 0.0], [1.0, 1.0]]},
            "properties": {"length_km": 141.0}
        });
        let result = summarize(&spec);
        assert!(result.contains("GeoJSON Feature (LineString)"));
        assert!(!result.contains("\""));
    }

    #[test]
    fn test_feature_with_top_level_id() {
        let spec = json!({
            "type": "Feature",
            "id": "district-42",
            "geometry": {"type": "Point", "coordinates": [0.0, 0.0]},
            "properties": {"pop": 1000}
        });
        let result = summarize(&spec);
        assert!(result.contains("GeoJSON Feature (Point): \"district-42\""));
    }

    #[test]
    fn test_feature_with_numeric_id() {
        let spec = json!({
            "type": "Feature",
            "id": 42,
            "geometry": {"type": "Point", "coordinates": [0.0, 0.0]},
            "properties": {}
        });
        let result = summarize(&spec);
        assert!(result.contains("GeoJSON Feature (Point): \"42\""));
    }

    #[test]
    fn test_feature_with_numeric_property_id() {
        let spec = json!({
            "type": "Feature",
            "geometry": {"type": "Point", "coordinates": [0.0, 0.0]},
            "properties": {"id": 99}
        });
        let result = summarize(&spec);
        assert!(result.contains("GeoJSON Feature (Point): \"99\""));
    }

    #[test]
    fn test_feature_unknown_geometry_casing() {
        // When geometry is null/missing, should say "Unknown" not "unknown"
        let spec = json!({
            "type": "Feature",
            "geometry": null,
            "properties": {"name": "test"}
        });
        let result = summarize(&spec);
        assert!(result.contains("GeoJSON Feature (Unknown)"));
    }

    // ── Bare geometry objects ───────────────────────────────────────

    #[test]
    fn test_point() {
        let spec = json!({
            "type": "Point",
            "coordinates": [-73.97, 40.77]
        });
        let result = summarize(&spec);
        assert!(result.contains("GeoJSON Point"));
        assert!(result.contains("lat=40.77"));
        assert!(result.contains("lon=-73.97"));
    }

    #[test]
    fn test_linestring() {
        let spec = json!({
            "type": "LineString",
            "coordinates": [[0.0, 0.0], [1.0, 1.0], [2.0, 0.0]]
        });
        let result = summarize(&spec);
        assert!(result.contains("GeoJSON LineString (3 coordinates)"));
    }

    #[test]
    fn test_polygon() {
        let spec = json!({
            "type": "Polygon",
            "coordinates": [[[0.0, 0.0], [4.0, 0.0], [4.0, 4.0], [0.0, 4.0], [0.0, 0.0]]]
        });
        let result = summarize(&spec);
        assert!(result.contains("GeoJSON Polygon (5 coordinates)"));
    }

    #[test]
    fn test_multipolygon() {
        let spec = json!({
            "type": "MultiPolygon",
            "coordinates": [
                [[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 0.0]]],
                [[[2.0, 2.0], [3.0, 2.0], [3.0, 3.0], [2.0, 2.0]]]
            ]
        });
        let result = summarize(&spec);
        assert!(result.contains("GeoJSON MultiPolygon (8 coordinates)"));
    }

    #[test]
    fn test_geometry_collection() {
        let spec = json!({
            "type": "GeometryCollection",
            "geometries": [
                {"type": "Point", "coordinates": [0.0, 0.0]},
                {"type": "LineString", "coordinates": [[0.0, 0.0], [1.0, 1.0]]}
            ]
        });
        let result = summarize(&spec);
        assert!(result.contains("GeoJSON GeometryCollection: 2 geometries [Point, LineString]"));
    }

    // ── Bbox computation ────────────────────────────────────────────

    #[test]
    fn test_computed_bbox() {
        let spec = json!({
            "type": "FeatureCollection",
            "features": [
                {
                    "type": "Feature",
                    "geometry": {"type": "Point", "coordinates": [-10.0, -20.0]},
                    "properties": {}
                },
                {
                    "type": "Feature",
                    "geometry": {"type": "Point", "coordinates": [30.0, 40.0]},
                    "properties": {}
                }
            ]
        });
        let result = summarize(&spec);
        assert!(result.contains("Bbox: [-10, -20, 30, 40]"));
    }

    // ── Edge cases ──────────────────────────────────────────────────

    #[test]
    fn test_unknown_type() {
        let spec = json!({"type": "Topology"});
        let result = summarize(&spec);
        assert!(result.contains("GeoJSON Topology"));
    }

    #[test]
    fn test_no_type() {
        let spec = json!({"coordinates": [0.0, 0.0]});
        let result = summarize(&spec);
        assert!(result.contains("GeoJSON Unknown"));
    }

    #[test]
    fn test_truncate_list() {
        let items: Vec<&str> = (0..15)
            .map(|i| match i {
                0 => "a",
                1 => "b",
                2 => "c",
                3 => "d",
                4 => "e",
                5 => "f",
                6 => "g",
                7 => "h",
                8 => "i",
                9 => "j",
                10 => "k",
                11 => "l",
                12 => "m",
                13 => "n",
                _ => "o",
            })
            .collect();
        let result = super::truncate_list(&items, 10);
        assert!(result.contains("+5 more"));
    }

    #[test]
    fn test_many_properties_truncated() {
        let mut props = serde_json::Map::new();
        for i in 0..15 {
            props.insert(format!("field_{i}"), json!(i));
        }
        let spec = json!({
            "type": "Feature",
            "geometry": {"type": "Point", "coordinates": [0.0, 0.0]},
            "properties": props
        });
        let result = summarize(&spec);
        assert!(result.contains("+5 more"));
    }
}
