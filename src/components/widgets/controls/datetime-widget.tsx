/**
 * Datetime widget - renders a datetime-local input field.
 *
 * Maps to ipywidgets DatetimeModel (UTC/timezone-aware) and NaiveDatetimeModel (local time).
 *
 * DatetimeModel: Values are in UTC. We convert to local time for display and back to UTC on change.
 * NaiveDatetimeModel: Values are in local time. No conversion needed.
 */

import { useCallback } from "react";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import {
  useWidgetModel,
  useWidgetModelValue,
  useWidgetStoreRequired,
} from "../widget-store-context";

type DatetimeValue =
  | {
      year: number;
      month: number;
      date: number;
      hours: number;
      minutes: number;
      seconds: number;
      milliseconds?: number;
    }
  | string
  | null;

/**
 * Create a Date from UTC components (for DatetimeModel).
 * The returned Date stores the time internally as UTC, so local getters will
 * automatically convert to the user's timezone.
 */
function utcComponentsToDate(value: Exclude<DatetimeValue, string | null>): Date {
  const date = new Date();
  date.setUTCFullYear(value.year, value.month, value.date);
  date.setUTCHours(value.hours, value.minutes, value.seconds, value.milliseconds ?? 0);
  return date;
}

/**
 * Create a Date from local components (for NaiveDatetimeModel).
 */
function localComponentsToDate(value: Exclude<DatetimeValue, string | null>): Date {
  const date = new Date();
  date.setFullYear(value.year, value.month, value.date);
  date.setHours(value.hours, value.minutes, value.seconds, value.milliseconds ?? 0);
  return date;
}

/**
 * Format a Date as datetime-local input string (YYYY-MM-DDTHH:MM).
 * Always uses local time getters since datetime-local displays in local time.
 */
function formatDateForInput(date: Date): string {
  const year = String(date.getFullYear()).padStart(4, "0");
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  const hours = String(date.getHours()).padStart(2, "0");
  const minutes = String(date.getMinutes()).padStart(2, "0");
  return `${year}-${month}-${day}T${hours}:${minutes}`;
}

/**
 * Convert datetime value to datetime-local input format (YYYY-MM-DDTHH:MM).
 * @param value - The datetime value from ipywidgets
 * @param isUtc - Whether the value is in UTC (DatetimeModel) or local time (NaiveDatetimeModel)
 */
function toDatetimeLocalString(value: DatetimeValue, isUtc: boolean): string {
  if (!value) return "";

  if (typeof value === "string") {
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) return "";
    return formatDateForInput(date);
  }

  // Convert components to Date, then format for display
  const date = isUtc ? utcComponentsToDate(value) : localComponentsToDate(value);
  return formatDateForInput(date);
}

/**
 * Extract UTC components from a Date (for DatetimeModel).
 */
function dateToUtcComponents(d: Date) {
  return {
    year: d.getUTCFullYear(),
    month: d.getUTCMonth(),
    date: d.getUTCDate(),
    hours: d.getUTCHours(),
    minutes: d.getUTCMinutes(),
    seconds: d.getUTCSeconds(),
    milliseconds: d.getUTCMilliseconds(),
  };
}

/**
 * Extract local components from a Date (for NaiveDatetimeModel).
 */
function dateToLocalComponents(d: Date) {
  return {
    year: d.getFullYear(),
    month: d.getMonth(),
    date: d.getDate(),
    hours: d.getHours(),
    minutes: d.getMinutes(),
    seconds: d.getSeconds(),
    milliseconds: d.getMilliseconds(),
  };
}

export function DatetimeWidget({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();
  const model = useWidgetModel(modelId);

  // Determine if this is a timezone-aware (UTC) model or naive (local) model
  const isUtc = model?.modelName === "DatetimeModel";

  // Subscribe to individual state keys
  const value = useWidgetModelValue<DatetimeValue>(modelId, "value") ?? null;
  const min = useWidgetModelValue<DatetimeValue>(modelId, "min") ?? null;
  const max = useWidgetModelValue<DatetimeValue>(modelId, "max") ?? null;
  const description = useWidgetModelValue<string>(modelId, "description");
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;

  const handleChange = useCallback(
    (e: React.ChangeEvent<HTMLInputElement>) => {
      const newValue = e.target.value;
      if (newValue) {
        // datetime-local input gives us a local time string
        const d = new Date(newValue);
        // Convert to appropriate format based on model type
        const components = isUtc ? dateToUtcComponents(d) : dateToLocalComponents(d);
        sendUpdate(modelId, { value: components });
      } else {
        sendUpdate(modelId, { value: null });
      }
    },
    [modelId, sendUpdate, isUtc],
  );

  return (
    <div
      className={cn("flex items-center gap-3", className)}
      data-widget-id={modelId}
      data-widget-type="Datetime"
    >
      {description && <Label className="shrink-0 text-sm">{description}</Label>}
      <Input
        type="datetime-local"
        value={toDatetimeLocalString(value, isUtc)}
        min={toDatetimeLocalString(min, isUtc)}
        max={toDatetimeLocalString(max, isUtc)}
        disabled={disabled}
        onChange={handleChange}
        className="w-52"
      />
    </div>
  );
}

export default DatetimeWidget;
