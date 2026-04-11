/**
 * Checkbox widget - renders a checkbox input.
 *
 * Maps to ipywidgets CheckboxModel.
 */

import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

export function CheckboxWidget({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();

  // Subscribe to individual state keys
  const value = useWidgetModelValue<boolean>(modelId, "value") ?? false;
  const description = useWidgetModelValue<string>(modelId, "description");
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;
  const indent = useWidgetModelValue<boolean>(modelId, "indent") ?? true;

  const handleChange = (checked: boolean | "indeterminate") => {
    if (checked !== "indeterminate") {
      sendUpdate(modelId, { value: checked });
    }
  };

  return (
    <div
      className={cn("flex items-center gap-2", indent && "pl-4", className)}
      data-widget-id={modelId}
      data-widget-type="Checkbox"
    >
      <Checkbox
        id={`checkbox-${modelId}`}
        checked={value}
        disabled={disabled}
        onCheckedChange={handleChange}
      />
      {description && (
        <Label
          htmlFor={`checkbox-${modelId}`}
          className={cn("text-sm", disabled && "opacity-50 cursor-not-allowed")}
        >
          {description}
        </Label>
      )}
    </div>
  );
}

export default CheckboxWidget;
