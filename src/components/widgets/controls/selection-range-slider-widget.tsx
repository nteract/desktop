/**
 * SelectionRangeSlider widget - range slider that selects from discrete options.
 *
 * Maps to ipywidgets SelectionRangeSliderModel.
 */

import { Label } from "@/components/ui/label";
import { Slider } from "@/components/ui/slider";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

export function SelectionRangeSliderWidget({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();

  // Subscribe to individual state keys
  const options = useWidgetModelValue<string[]>(modelId, "_options_labels") ?? [];
  const index = useWidgetModelValue<number[]>(modelId, "index") ?? [0, 0];
  const description = useWidgetModelValue<string>(modelId, "description");
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;
  const orientation =
    useWidgetModelValue<"horizontal" | "vertical">(modelId, "orientation") ?? "horizontal";
  const readout = useWidgetModelValue<boolean>(modelId, "readout") ?? true;
  const continuousUpdate = useWidgetModelValue<boolean>(modelId, "continuous_update") ?? true;

  const [lowIndex, highIndex] = index;

  const handleChange = (newValue: number[]) => {
    const newLow = Math.round(newValue[0]);
    const newHigh = Math.round(newValue[1]);
    const clampedLow = Math.min(options.length - 1, Math.max(0, newLow));
    const clampedHigh = Math.min(options.length - 1, Math.max(0, newHigh));

    if (continuousUpdate || clampedLow !== lowIndex || clampedHigh !== highIndex) {
      sendUpdate(modelId, { index: [clampedLow, clampedHigh] });
    }
  };

  const isVertical = orientation === "vertical";
  const lowLabel = options[lowIndex] ?? "";
  const highLabel = options[highIndex] ?? "";

  return (
    <div
      className={cn("flex gap-3", isVertical ? "flex-col items-center" : "items-center", className)}
      data-widget-id={modelId}
      data-widget-type="SelectionRangeSlider"
    >
      {description && <Label className="shrink-0 text-sm">{description}</Label>}
      <Slider
        value={[lowIndex, highIndex]}
        min={0}
        max={Math.max(0, options.length - 1)}
        step={1}
        disabled={disabled || options.length === 0}
        orientation={orientation}
        onValueChange={handleChange}
        className={isVertical ? "h-32" : "min-w-24 flex-1"}
      />
      {readout && (
        <span className="min-w-24 text-sm text-muted-foreground">
          {lowLabel} - {highLabel}
        </span>
      )}
    </div>
  );
}

export default SelectionRangeSliderWidget;
