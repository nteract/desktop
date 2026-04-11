/**
 * DatePicker widget - renders a date input field.
 *
 * Maps to ipywidgets DatePickerModel.
 */

import { useCallback } from "react";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

type IpyDate = { year: number; month: number; date: number } | null;

// Convert ipywidgets date object to YYYY-MM-DD format for input
function toDateString(value: IpyDate): string {
  if (!value) return "";
  const year = String(value.year).padStart(4, "0");
  const month = String(value.month + 1).padStart(2, "0");
  const date = String(value.date).padStart(2, "0");
  return `${year}-${month}-${date}`;
}

export function DatePickerWidget({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();

  // Subscribe to individual state keys
  const value = useWidgetModelValue<IpyDate>(modelId, "value") ?? null;
  const min = useWidgetModelValue<IpyDate>(modelId, "min") ?? null;
  const max = useWidgetModelValue<IpyDate>(modelId, "max") ?? null;
  const description = useWidgetModelValue<string>(modelId, "description");
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;

  const handleChange = useCallback(
    (e: React.ChangeEvent<HTMLInputElement>) => {
      const newValue = e.target.value;
      if (newValue) {
        const [year, month, day] = newValue.split("-").map(Number);
        sendUpdate(modelId, { value: { year, month: month - 1, date: day } });
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
      data-widget-type="DatePicker"
    >
      {description && <Label className="shrink-0 text-sm">{description}</Label>}
      <Input
        type="date"
        value={toDateString(value)}
        min={toDateString(min)}
        max={toDateString(max)}
        disabled={disabled}
        onChange={handleChange}
        className="w-40"
      />
    </div>
  );
}

export default DatePickerWidget;
