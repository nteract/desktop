import type { CSSProperties } from "react";
import { useMemo } from "react";
import {
  extractChildGridStyles,
  extractContainerGridStyles,
  extractGeneralStyles,
  hasContainerGridStyles,
} from "./layout-utils";
import type { WidgetModel } from "./widget-store";
import { useResolvedModelValue } from "./widget-store-context";

/**
 * Hook to extract CSS styles from a widget's Layout model.
 *
 * ipywidgets widgets have a `layout` property that references a LayoutModel.
 * This hook resolves that reference and extracts CSS properties for styling.
 *
 * @param modelId - The widget model ID
 * @returns Object with containerStyle (for grid containers) and childStyle (for grid_area positioning)
 */
export function useLayoutStyles(modelId: string): {
  /** CSS grid container styles (grid_template_columns, grid_template_rows, etc.) + general styles */
  containerStyle: CSSProperties;
  /** CSS child placement styles (grid_area, grid_row, grid_column) */
  childStyle: CSSProperties;
  /** Whether the layout has grid container properties */
  hasGridLayout: boolean;
} {
  // Resolve the layout model reference (IPY_MODEL_xxx -> LayoutModel)
  const layoutModel = useResolvedModelValue(modelId, "layout") as WidgetModel | undefined;

  return useMemo(() => {
    if (!layoutModel?.state) {
      return {
        containerStyle: {},
        childStyle: {},
        hasGridLayout: false,
      };
    }

    const state = layoutModel.state;

    // Extract different style categories
    const gridContainerStyles = extractContainerGridStyles(state);
    const generalStyles = extractGeneralStyles(state);
    const childStyles = extractChildGridStyles(state);

    // Combine container grid styles with general styles for the container
    const containerStyle: CSSProperties = {
      ...generalStyles,
      ...gridContainerStyles,
    };

    return {
      containerStyle,
      childStyle: childStyles,
      hasGridLayout: hasContainerGridStyles(state),
    };
  }, [layoutModel]);
}
