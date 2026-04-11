/**
 * GridBox widget - CSS grid container.
 *
 * Maps to ipywidgets GridBoxModel. Arranges children in a CSS grid layout.
 * Default is a responsive 2-column grid.
 */

import { cn } from "@/lib/utils";
import { useLayoutStyles } from "../use-layout-styles";
import type { WidgetComponentProps } from "../widget-registry";
import { parseModelRef, useWidgetModelValue } from "../widget-store-context";
import { WidgetView } from "../widget-view";

// Map ipywidgets box_style to Tailwind classes
const BOX_STYLE_MAP: Record<string, string> = {
  "": "",
  primary: "border border-blue-500 bg-blue-50/50 dark:bg-blue-950/50 rounded-md p-2",
  success: "border border-green-500 bg-green-50/50 dark:bg-green-950/50 rounded-md p-2",
  info: "border border-sky-500 bg-sky-50/50 dark:bg-sky-950/50 rounded-md p-2",
  warning: "border border-yellow-500 bg-yellow-50/50 dark:bg-yellow-950/50 rounded-md p-2",
  danger: "border border-red-500 bg-red-50/50 dark:bg-red-950/50 rounded-md p-2",
};

export function GridBoxWidget({ modelId, className }: WidgetComponentProps) {
  // Subscribe to individual state keys
  const children = useWidgetModelValue<string[]>(modelId, "children");
  const boxStyle = useWidgetModelValue<string>(modelId, "box_style") ?? "";

  // Get layout styles from the Layout model
  const { containerStyle, hasGridLayout } = useLayoutStyles(modelId);

  const styleClass = BOX_STYLE_MAP[boxStyle] ?? "";

  return (
    <div
      className={cn(
        "grid",
        // Only apply default Tailwind grid classes if no layout grid is specified
        !hasGridLayout && "grid-cols-1 sm:grid-cols-2 gap-2",
        styleClass,
        className,
      )}
      style={containerStyle}
      data-widget-id={modelId}
      data-widget-type="GridBox"
    >
      {children?.map((childRef) => {
        const childId = parseModelRef(childRef);
        return childId ? <WidgetView key={childId} modelId={childId} /> : null;
      })}
    </div>
  );
}

export default GridBoxWidget;
