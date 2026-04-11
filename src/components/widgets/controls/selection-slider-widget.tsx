/**
 * SelectionSlider widget - slider that selects from discrete options.
 *
 * Maps to ipywidgets SelectionSliderModel.
 */

import { Label } from "@/components/ui/label";
import { Slider } from "@/components/ui/slider";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

export function SelectionSliderWidget({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();

  // Subscribe to individual state keys
  const options = useWidgetModelValue<string[]>(modelId, "_options_labels") ?? [];
  const index = useWidgetModelValue<number>(modelId, "index") ?? 0;
  const description = useWidgetModelValue<string>(modelId, "description");
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;
  const orientation =
    useWidgetModelValue<"horizontal" | "vertical">(modelId, "orientation") ?? "horizontal";
  const readout = useWidgetModelValue<boolean>(modelId, "readout") ?? true;
  const continuousUpdate = useWidgetModelValue<boolean>(modelId, "continuous_update") ?? true;

  const handleChange = (newValue: number[]) => {
    const newIndex = Math.round(newValue[0]);
    const clampedIndex = Math.min(options.length - 1, Math.max(0, newIndex));
    if (continuousUpdate || clampedIndex !== index) {
      sendUpdate(modelId, { index: clampedIndex });
    }
  };

  const isVertical = orientation === "vertical";
  const currentLabel = options[index] ?? "";

  return (
    <div
      className={cn("flex gap-3", isVertical ? "flex-col items-center" : "items-center", className)}
      data-widget-id={modelId}
      data-widget-type="SelectionSlider"
    >
      {description && <Label className="shrink-0 text-sm">{description}</Label>}
      <Slider
        value={[index]}
        min={0}
        max={Math.max(0, options.length - 1)}
        step={1}
        disabled={disabled || options.length === 0}
        orientation={orientation}
        onValueChange={handleChange}
        className={isVertical ? "h-32" : "min-w-24 flex-1"}
      />
      {readout && <span className="min-w-16 text-sm text-muted-foreground">{currentLabel}</span>}
    </div>
  );
}

export default SelectionSliderWidget;
