import type { CSSProperties } from "react";

/**
 * Utilities for converting ipywidgets Layout model properties to CSS.
 *
 * ipywidgets uses snake_case for CSS properties (e.g., grid_template_columns),
 * which need to be converted to camelCase for React's style prop.
 */

/**
 * Map of ipywidgets Layout model property names to React CSS property names.
 * Only includes properties that need explicit mapping (non-trivial conversions).
 */
const LAYOUT_CSS_MAP: Record<string, string> = {
  // Grid container properties
  grid_template_columns: "gridTemplateColumns",
  grid_template_rows: "gridTemplateRows",
  grid_template_areas: "gridTemplateAreas",
  grid_auto_columns: "gridAutoColumns",
  grid_auto_rows: "gridAutoRows",
  grid_auto_flow: "gridAutoFlow",
  grid_gap: "gridGap",
  // Grid child placement properties
  grid_row: "gridRow",
  grid_column: "gridColumn",
  grid_area: "gridArea",
  // Size properties
  min_width: "minWidth",
  max_width: "maxWidth",
  min_height: "minHeight",
  max_height: "maxHeight",
  // Flexbox properties
  flex_flow: "flexFlow",
  align_items: "alignItems",
  align_self: "alignSelf",
  align_content: "alignContent",
  justify_content: "justifyContent",
  justify_items: "justifyItems",
  // Text properties
  object_fit: "objectFit",
  object_position: "objectPosition",
  // Overflow
  overflow_x: "overflowX",
  overflow_y: "overflowY",
};

/**
 * Grid container properties that apply to the parent grid element.
 */
const CONTAINER_GRID_PROPERTIES = new Set([
  "grid_template_columns",
  "grid_template_rows",
  "grid_template_areas",
  "grid_auto_columns",
  "grid_auto_rows",
  "grid_auto_flow",
  "grid_gap",
]);

/**
 * Child placement properties that apply to grid children.
 */
const CHILD_GRID_PROPERTIES = new Set(["grid_row", "grid_column", "grid_area"]);

/**
 * General layout properties that can apply to any element.
 */
const GENERAL_PROPERTIES = new Set([
  "width",
  "height",
  "min_width",
  "max_width",
  "min_height",
  "max_height",
  "margin",
  "padding",
  "border",
  "overflow",
  "overflow_x",
  "overflow_y",
  "visibility",
  "display",
  "flex",
  "flex_flow",
  "align_items",
  "align_self",
  "align_content",
  "justify_content",
  "justify_items",
  "order",
  "object_fit",
  "object_position",
]);

/**
 * Convert a snake_case property name to camelCase.
 */
function snakeToCamelCase(str: string): string {
  return str.replace(/_([a-z])/g, (_, letter) => letter.toUpperCase());
}

/**
 * Convert an ipywidgets Layout property name to a React CSS property name.
 */
function toReactCSSProperty(snakeCase: string): string {
  return LAYOUT_CSS_MAP[snakeCase] ?? snakeToCamelCase(snakeCase);
}

/**
 * Extract CSS properties from a Layout model state.
 *
 * @param state - The Layout model's state object
 * @param propertyFilter - Optional set of property names to include
 * @returns React CSSProperties object
 */
export function layoutStateToCSS(
  state: Record<string, unknown>,
  propertyFilter?: Set<string>,
): CSSProperties {
  const style: Record<string, string> = {};

  for (const [key, value] of Object.entries(state)) {
    // Skip internal ipywidgets properties
    if (key.startsWith("_")) continue;
    // Skip null, undefined, or empty string values
    if (value === null || value === undefined || value === "") continue;
    // Apply filter if provided
    if (propertyFilter && !propertyFilter.has(key)) continue;
    // Only include string values (CSS values)
    if (typeof value !== "string") continue;

    const cssProperty = toReactCSSProperty(key);
    style[cssProperty] = value;
  }

  return style as CSSProperties;
}

/**
 * Extract container grid CSS properties from a Layout model state.
 * These properties should be applied to grid container elements.
 */
export function extractContainerGridStyles(state: Record<string, unknown>): CSSProperties {
  return layoutStateToCSS(state, CONTAINER_GRID_PROPERTIES);
}

/**
 * Extract child placement CSS properties from a Layout model state.
 * These properties should be applied to grid children for positioning.
 */
export function extractChildGridStyles(state: Record<string, unknown>): CSSProperties {
  return layoutStateToCSS(state, CHILD_GRID_PROPERTIES);
}

/**
 * Extract general layout CSS properties from a Layout model state.
 * These can be applied to any element (width, height, margin, etc.).
 */
export function extractGeneralStyles(state: Record<string, unknown>): CSSProperties {
  return layoutStateToCSS(state, GENERAL_PROPERTIES);
}

/**
 * Check if a Layout model state has any grid container properties.
 */
export function hasContainerGridStyles(state: Record<string, unknown>): boolean {
  for (const key of CONTAINER_GRID_PROPERTIES) {
    const value = state[key];
    if (value !== null && value !== undefined && value !== "") {
      return true;
    }
  }
  return false;
}
