/**
 * TimePicker widget - renders a time input field.
 *
 * Maps to ipywidgets TimePickerModel.
 */

import { useCallback } from "react";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

// Convert time object or string to HH:MM format for input
function toTimeString(
  value: { hours: number; minutes: number; seconds?: number } | string | null,
): string {
  if (!value) return "";
  if (typeof value === "string") return value;
  const hours = String(value.hours).padStart(2, "0");
  const minutes = String(value.minutes).padStart(2, "0");
  return `${hours}:${minutes}`;
}

export function TimePickerWidget({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();

  // Subscribe to individual state keys
  const value =
    useWidgetModelValue<{ hours: number; minutes: number; seconds?: number } | string | null>(
      modelId,
      "value",
    ) ?? null;
  const min =
    useWidgetModelValue<{ hours: number; minutes: number } | string | null>(modelId, "min") ?? null;
  const max =
    useWidgetModelValue<{ hours: number; minutes: number } | string | null>(modelId, "max") ?? null;
  const description = useWidgetModelValue<string>(modelId, "description");
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;

  const handleChange = useCallback(
    (e: React.ChangeEvent<HTMLInputElement>) => {
      const newValue = e.target.value;
      if (newValue) {
        const [hours, minutes] = newValue.split(":").map(Number);
        sendUpdate(modelId, {
          value: { hours, minutes, seconds: 0, milliseconds: 0 },
        });
      } else {
        sendUpdate(modelId, { value: null });
      }
    },
    [modelId, sendUpdate],
  );

  return (
    <div
      className={cn("flex items-center gap-3", className)}
      data-widget-id={modelId}
      data-widget-type="TimePicker"
    >
      {description && <Label className="shrink-0 text-sm">{description}</Label>}
      <Input
        type="time"
        value={toTimeString(value)}
        min={toTimeString(min)}
        max={toTimeString(max)}
        disabled={disabled}
        onChange={handleChange}
        className="w-32"
      />
    </div>
  );
}

export default TimePickerWidget;
