/**
 * ToggleButton widget - renders a single toggle button.
 *
 * Maps to ipywidgets ToggleButtonModel.
 */

import { Toggle } from "@/components/ui/toggle";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

export function ToggleButtonWidget({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();

  // Subscribe to individual state keys
  const value = useWidgetModelValue<boolean>(modelId, "value") ?? false;
  const description = useWidgetModelValue<string>(modelId, "description") ?? "";
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;
  const icon = useWidgetModelValue<string>(modelId, "icon");
  const buttonStyle = useWidgetModelValue<string>(modelId, "button_style") ?? "";

  const handlePressedChange = (pressed: boolean) => {
    sendUpdate(modelId, { value: pressed });
  };

  // Map button_style to variant - outline for styled buttons
  const variant = buttonStyle ? "outline" : "default";

  return (
    <Toggle
      pressed={value}
      onPressedChange={handlePressedChange}
      disabled={disabled}
      variant={variant}
      className={cn(className)}
      data-widget-id={modelId}
      data-widget-type="ToggleButton"
    >
      {icon && <span className="mr-1">{icon}</span>}
      {description || "Toggle"}
    </Toggle>
  );
}

export default ToggleButtonWidget;
