/**
 * IntSlider widget - renders an integer slider.
 *
 * Maps to ipywidgets IntSliderModel.
 */

import { Label } from "@/components/ui/label";
import { Slider } from "@/components/ui/slider";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

export function IntSlider({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();

  // Subscribe to individual state keys for fine-grained updates
  const value = useWidgetModelValue<number>(modelId, "value") ?? 0;
  const min = useWidgetModelValue<number>(modelId, "min") ?? 0;
  const max = useWidgetModelValue<number>(modelId, "max") ?? 100;
  const step = useWidgetModelValue<number>(modelId, "step") ?? 1;
  const description = useWidgetModelValue<string>(modelId, "description");
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;
  const orientation =
    useWidgetModelValue<"horizontal" | "vertical">(modelId, "orientation") ?? "horizontal";
  const readout = useWidgetModelValue<boolean>(modelId, "readout") ?? true;

  const handleChange = (newValue: number[]) => {
    // Round to step and clamp to range
    const v = Math.round(newValue[0] / step) * step;
    const clamped = Math.min(max, Math.max(min, v));
    sendUpdate(modelId, { value: clamped });
  };

  const isVertical = orientation === "vertical";

  return (
    <div
      className={cn("flex gap-3", isVertical ? "flex-col items-center" : "items-center", className)}
      data-widget-id={modelId}
      data-widget-type="IntSlider"
    >
      {description && <Label className="shrink-0 text-sm">{description}</Label>}
      <Slider
        value={[value]}
        min={min}
        max={max}
        step={step}
        disabled={disabled}
        orientation={orientation}
        onValueChange={handleChange}
        className={isVertical ? "h-32" : "flex-1 min-w-24"}
      />
      {readout && (
        <span className="w-12 text-right tabular-nums text-sm text-muted-foreground">{value}</span>
      )}
    </div>
  );
}

export default IntSlider;
