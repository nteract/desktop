/**
 * VBox widget - vertical flex container.
 *
 * Maps to ipywidgets VBoxModel. Arranges children in a vertical column.
 */

import { cn } from "@/lib/utils";
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

export function VBoxWidget({ modelId, className }: WidgetComponentProps) {
  // Subscribe to individual state keys
  const children = useWidgetModelValue<string[]>(modelId, "children");
  const boxStyle = useWidgetModelValue<string>(modelId, "box_style") ?? "";

  const styleClass = BOX_STYLE_MAP[boxStyle] ?? "";

  return (
    <div
      className={cn("flex flex-col gap-2", styleClass, className)}
      data-widget-id={modelId}
      data-widget-type="VBox"
    >
      {children?.map((childRef) => {
        const childId = parseModelRef(childRef);
        return childId ? <WidgetView key={childId} modelId={childId} /> : null;
      })}
    </div>
  );
}

export default VBoxWidget;
