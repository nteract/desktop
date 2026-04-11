/**
 * IntText widget - renders a numeric text input for integers.
 *
 * Maps to ipywidgets IntTextModel.
 */

import { useCallback, useEffect, useState } from "react";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

export function IntTextWidget({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();

  // Subscribe to individual state keys
  const value = useWidgetModelValue<number>(modelId, "value") ?? 0;
  const min = useWidgetModelValue<number>(modelId, "min");
  const max = useWidgetModelValue<number>(modelId, "max");
  const step = useWidgetModelValue<number>(modelId, "step") ?? 1;
  const description = useWidgetModelValue<string>(modelId, "description");
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;
  const continuousUpdate = useWidgetModelValue<boolean>(modelId, "continuous_update") ?? false;

  // Local state for the input value (as string for editing)
  const [localValue, setLocalValue] = useState(String(value));

  // Sync local state when value changes from kernel
  useEffect(() => {
    setLocalValue(String(value));
  }, [value]);

  const clampValue = useCallback(
    (val: number): number => {
      let clamped = val;
      if (min !== undefined && min !== null) {
        clamped = Math.max(min, clamped);
      }
      if (max !== undefined && max !== null) {
        clamped = Math.min(max, clamped);
      }
      return clamped;
    },
    [min, max],
  );

  const handleChange = useCallback(
    (e: React.ChangeEvent<HTMLInputElement>) => {
      const newValue = e.target.value;
      setLocalValue(newValue);

      if (continuousUpdate) {
        const parsed = parseInt(newValue, 10);
        if (!Number.isNaN(parsed)) {
          const clamped = clampValue(parsed);
          sendUpdate(modelId, { value: clamped });
        }
      }
    },
    [modelId, continuousUpdate, clampValue, sendUpdate],
  );

  const handleBlur = useCallback(() => {
    const parsed = parseInt(localValue, 10);
    if (!Number.isNaN(parsed)) {
      const clamped = clampValue(parsed);
      setLocalValue(String(clamped));
      if (clamped !== value) {
        sendUpdate(modelId, { value: clamped });
      }
    } else {
      // Reset to current value if invalid
      setLocalValue(String(value));
    }
  }, [modelId, localValue, value, clampValue, sendUpdate]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === "Enter") {
        const parsed = parseInt(localValue, 10);
        if (!Number.isNaN(parsed)) {
          const clamped = clampValue(parsed);
          setLocalValue(String(clamped));
          sendUpdate(modelId, { value: clamped });
        }
      }
    },
    [modelId, localValue, clampValue, sendUpdate],
  );

  return (
    <div
      className={cn("flex items-center gap-3", className)}
      data-widget-id={modelId}
      data-widget-type="IntText"
    >
      {description && <Label className="shrink-0 text-sm">{description}</Label>}
      <Input
        type="number"
        value={localValue}
        disabled={disabled}
        step={step}
        min={min}
        max={max}
        onChange={handleChange}
        onBlur={handleBlur}
        onKeyDown={handleKeyDown}
        className="w-24"
      />
    </div>
  );
}

export default IntTextWidget;
