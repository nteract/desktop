/**
 * FloatLogSlider widget - renders a logarithmic scale slider.
 *
 * Maps to ipywidgets FloatLogSliderModel.
 */

import { Label } from "@/components/ui/label";
import { Slider } from "@/components/ui/slider";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

export function FloatLogSlider({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();

  // Subscribe to individual state keys
  const value = useWidgetModelValue<number>(modelId, "value") ?? 1;
  const base = useWidgetModelValue<number>(modelId, "base") ?? 10;
  const min = useWidgetModelValue<number>(modelId, "min") ?? 0; // exponent min
  const max = useWidgetModelValue<number>(modelId, "max") ?? 4; // exponent max
  const step = useWidgetModelValue<number>(modelId, "step") ?? 0.1;
  const description = useWidgetModelValue<string>(modelId, "description");
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;
  const orientation =
    useWidgetModelValue<"horizontal" | "vertical">(modelId, "orientation") ?? "horizontal";
  const readout = useWidgetModelValue<boolean>(modelId, "readout") ?? true;
  const readoutFormat = useWidgetModelValue<string>(modelId, "readout_format") ?? ".3g";

  // Convert value to log scale position (exponent)
  const valueToExponent = (v: number): number => {
    if (v <= 0) return min;
    return Math.log(v) / Math.log(base);
  };

  // Convert exponent to actual value
  const exponentToValue = (exp: number): number => {
    return base ** exp;
  };

  const currentExponent = valueToExponent(value);

  const handleChange = (newValue: number[]) => {
    const newExponent = Math.min(max, Math.max(min, newValue[0]));
    const newActualValue = exponentToValue(newExponent);
    sendUpdate(modelId, { value: newActualValue });
  };

  // Format value for display
  const formatValue = (v: number): string => {
    // Parse Python-style format spec (e.g., ".3g")
    const matchG = readoutFormat.match(/\.(\d+)g/);
    if (matchG) {
      return v.toPrecision(parseInt(matchG[1], 10));
    }
    const matchF = readoutFormat.match(/\.(\d+)f/);
    if (matchF) {
      return v.toFixed(parseInt(matchF[1], 10));
    }
    const matchE = readoutFormat.match(/\.(\d+)e/);
    if (matchE) {
      return v.toExponential(parseInt(matchE[1], 10));
    }
    return String(v);
  };

  const isVertical = orientation === "vertical";

  return (
    <div
      className={cn("flex gap-3", isVertical ? "flex-col items-center" : "items-center", className)}
      data-widget-id={modelId}
      data-widget-type="FloatLogSlider"
    >
      {description && <Label className="shrink-0 text-sm">{description}</Label>}
      <Slider
        value={[currentExponent]}
        min={min}
        max={max}
        step={step}
        disabled={disabled}
        orientation={orientation}
        onValueChange={handleChange}
        className={isVertical ? "h-32" : "min-w-24 flex-1"}
      />
      {readout && (
        <span className="w-20 text-right text-sm tabular-nums text-muted-foreground">
          {formatValue(value)}
        </span>
      )}
    </div>
  );
}

export default FloatLogSlider;
