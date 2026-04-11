/**
 * ControllerButton widget - displays a gamepad button state.
 *
 * Maps to ipywidgets ControllerButtonModel.
 */

import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue } from "../widget-store-context";

export function ControllerButtonWidget({ modelId, className }: WidgetComponentProps) {
  const pressed = useWidgetModelValue<boolean>(modelId, "pressed") ?? false;
  const value = useWidgetModelValue<number>(modelId, "value") ?? 0;

  return (
    <div
      className={cn(
        "flex size-8 items-center justify-center rounded-full border-2 text-xs font-medium transition-colors",
        pressed
          ? "border-primary bg-primary text-primary-foreground"
          : "border-muted-foreground/30 bg-muted text-muted-foreground",
        className,
      )}
      data-widget-id={modelId}
      data-widget-type="ControllerButton"
      title={`Value: ${value.toFixed(2)}`}
    >
      {pressed ? "●" : "○"}
    </div>
  );
}

export default ControllerButtonWidget;
