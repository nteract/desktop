/**
 * ColorPicker widget - renders a color selection input.
 *
 * Maps to ipywidgets ColorPickerModel.
 */

import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

export function ColorPicker({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();

  const value = useWidgetModelValue<string>(modelId, "value") ?? "#000000";
  const description = useWidgetModelValue<string>(modelId, "description");
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;
  const concise = useWidgetModelValue<boolean>(modelId, "concise") ?? false;

  const handleChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    sendUpdate(modelId, { value: e.target.value });
  };

  return (
    <div
      className={cn("flex items-center gap-3", className)}
      data-widget-id={modelId}
      data-widget-type="ColorPicker"
    >
      {description && <Label className="shrink-0 text-sm">{description}</Label>}
      <input
        type="color"
        value={value}
        disabled={disabled}
        onChange={handleChange}
        className={cn(
          "h-8 w-12 cursor-pointer rounded border border-input bg-transparent p-0.5",
          "disabled:cursor-not-allowed disabled:opacity-50",
        )}
      />
      {!concise && <span className="text-sm text-muted-foreground font-mono">{value}</span>}
    </div>
  );
}

export default ColorPicker;
