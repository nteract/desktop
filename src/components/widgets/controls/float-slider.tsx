/**
 * FloatSlider widget - renders a floating point slider.
 *
 * Maps to ipywidgets FloatSliderModel.
 */

import { Label } from "@/components/ui/label";
import { Slider } from "@/components/ui/slider";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

export function FloatSlider({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();

  // Subscribe to individual state keys for fine-grained updates
  const value = useWidgetModelValue<number>(modelId, "value") ?? 0;
  const min = useWidgetModelValue<number>(modelId, "min") ?? 0;
  const max = useWidgetModelValue<number>(modelId, "max") ?? 100;
  const step = useWidgetModelValue<number>(modelId, "step") ?? 0.1;
  const description = useWidgetModelValue<string>(modelId, "description");
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;
  const orientation =
    useWidgetModelValue<"horizontal" | "vertical">(modelId, "orientation") ?? "horizontal";
  const readout = useWidgetModelValue<boolean>(modelId, "readout") ?? true;
  const readoutFormat = useWidgetModelValue<string>(modelId, "readout_format") ?? ".2f";

  const handleChange = (newValue: number[]) => {
    // Clamp to range (no integer rounding for floats)
    const clamped = Math.min(max, Math.max(min, newValue[0]));
    sendUpdate(modelId, { value: clamped });
  };

  // Format value for display based on readout_format
  const formatValue = (v: number): string => {
    // Parse Python-style format spec (e.g., ".2f")
    const match = readoutFormat.match(/\.(\d+)f/);
    if (match) {
      return v.toFixed(parseInt(match[1], 10));
    }
    return String(v);
  };

  const isVertical = orientation === "vertical";

  return (
    <div
      className={cn("flex gap-3", isVertical ? "flex-col items-center" : "items-center", className)}
      data-widget-id={modelId}
      data-widget-type="FloatSlider"
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
        <span className="w-16 text-right tabular-nums text-sm text-muted-foreground">
          {formatValue(value)}
        </span>
      )}
    </div>
  );
}

export default FloatSlider;
