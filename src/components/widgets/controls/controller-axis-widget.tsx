/**
 * ControllerAxis widget - displays a gamepad axis value.
 *
 * Maps to ipywidgets ControllerAxisModel.
 */

import { Progress } from "@/components/ui/progress";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue } from "../widget-store-context";

export function ControllerAxisWidget({ modelId, className }: WidgetComponentProps) {
  // Axis value ranges from -1 to 1
  const value = useWidgetModelValue<number>(modelId, "value") ?? 0;

  // Convert -1..1 to 0..100 for progress display
  const progressValue = ((value + 1) / 2) * 100;

  return (
    <div
      className={cn("flex items-center gap-2", className)}
      data-widget-id={modelId}
      data-widget-type="ControllerAxis"
    >
      <Progress value={progressValue} className="h-2 w-16" />
      <span className="w-12 text-right text-xs tabular-nums text-muted-foreground">
        {value.toFixed(2)}
      </span>
    </div>
  );
}

export default ControllerAxisWidget;
