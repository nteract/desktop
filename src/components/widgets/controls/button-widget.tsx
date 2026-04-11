/**
 * Button widget - renders a clickable button.
 *
 * Maps to ipywidgets ButtonModel.
 */

import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";
import { getButtonStyle } from "./button-style-utils";

export function ButtonWidget({ modelId, className }: WidgetComponentProps) {
  const { sendCustom } = useWidgetStoreRequired();

  // Subscribe to individual state keys
  const description = useWidgetModelValue<string>(modelId, "description") ?? "";
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;
  const buttonStyle = useWidgetModelValue<string>(modelId, "button_style") ?? "";
  const icon = useWidgetModelValue<string>(modelId, "icon");
  const tooltip = useWidgetModelValue<string>(modelId, "tooltip");

  const handleClick = () => {
    // Send click event to kernel
    sendCustom(modelId, { event: "click" });
  };

  const { variant, className: styleClassName } = getButtonStyle(buttonStyle);

  return (
    <Button
      variant={variant}
      disabled={disabled}
      onClick={handleClick}
      title={tooltip}
      className={cn(styleClassName, className)}
      data-widget-id={modelId}
      data-widget-type="Button"
    >
      {icon && <span className="mr-1">{icon}</span>}
      {description || "Button"}
    </Button>
  );
}

export default ButtonWidget;
