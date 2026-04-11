/**
 * Stack widget - single-child container showing one child at a time.
 *
 * Maps to ipywidgets StackModel. Like Tab but without the tab headers —
 * displays only the child at `selected_index`.
 */

import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { parseModelRef, useWidgetModelValue } from "../widget-store-context";
import { WidgetView } from "../widget-view";

export function StackWidget({ modelId, className }: WidgetComponentProps) {
  // Subscribe to individual state keys
  const children = useWidgetModelValue<string[]>(modelId, "children");
  const selectedIndex = useWidgetModelValue<number>(modelId, "selected_index") ?? 0;

  if (!children || children.length === 0) return null;

  const selectedRef = children[selectedIndex];
  if (!selectedRef) return null;

  const childId = parseModelRef(selectedRef);
  if (!childId) return null;

  return (
    <div className={cn("w-full", className)} data-widget-id={modelId} data-widget-type="Stack">
      <WidgetView modelId={childId} />
    </div>
  );
}

export default StackWidget;
