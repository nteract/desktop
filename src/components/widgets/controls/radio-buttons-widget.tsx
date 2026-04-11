/**
 * RadioButtons widget - renders a group of radio buttons.
 *
 * Maps to ipywidgets RadioButtonsModel.
 */

import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

export function RadioButtonsWidget({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();

  // Subscribe to individual state keys
  const options = useWidgetModelValue<string[]>(modelId, "_options_labels") ?? [];
  const index = useWidgetModelValue<number | null>(modelId, "index");
  const description = useWidgetModelValue<string>(modelId, "description");
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;

  // Convert index to string value for RadioGroup
  const value = index !== null && index !== undefined && index >= 0 ? String(index) : undefined;

  const handleValueChange = (newValue: string) => {
    const newIndex = parseInt(newValue, 10);
    if (!Number.isNaN(newIndex)) {
      sendUpdate(modelId, { index: newIndex });
    }
  };

  return (
    <div
      className={cn("flex items-start gap-3", className)}
      data-widget-id={modelId}
      data-widget-type="RadioButtons"
    >
      {description && <Label className="shrink-0 text-sm pt-0.5">{description}</Label>}
      <RadioGroup value={value} onValueChange={handleValueChange} disabled={disabled}>
        {options.map((option, idx) => (
          <div key={idx} className="flex items-center gap-2">
            <RadioGroupItem
              value={String(idx)}
              id={`${modelId}-radio-${idx}`}
              disabled={disabled}
            />
            <Label
              htmlFor={`${modelId}-radio-${idx}`}
              className={cn(
                "text-sm font-normal cursor-pointer",
                disabled && "opacity-50 cursor-not-allowed",
              )}
            >
              {option}
            </Label>
          </div>
        ))}
      </RadioGroup>
    </div>
  );
}

export default RadioButtonsWidget;
