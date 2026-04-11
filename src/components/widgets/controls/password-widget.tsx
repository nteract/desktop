/**
 * Password widget - renders a masked text input field.
 *
 * Maps to ipywidgets PasswordModel.
 */

import { useCallback, useEffect, useState } from "react";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

export function PasswordWidget({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate, sendCustom } = useWidgetStoreRequired();

  // Subscribe to individual state keys
  const value = useWidgetModelValue<string>(modelId, "value") ?? "";
  const description = useWidgetModelValue<string>(modelId, "description");
  const placeholder = useWidgetModelValue<string>(modelId, "placeholder") ?? "";
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;
  const continuousUpdate = useWidgetModelValue<boolean>(modelId, "continuous_update") ?? true;

  // Local state for non-continuous updates
  const [localValue, setLocalValue] = useState(value);

  // Sync local state when value changes from kernel
  useEffect(() => {
    setLocalValue(value);
  }, [value]);

  const handleChange = useCallback(
    (e: React.ChangeEvent<HTMLInputElement>) => {
      const newValue = e.target.value;
      setLocalValue(newValue);

      if (continuousUpdate) {
        sendUpdate(modelId, { value: newValue });
      }
    },
    [modelId, continuousUpdate, sendUpdate],
  );

  const handleBlur = useCallback(() => {
    if (!continuousUpdate && localValue !== value) {
      sendUpdate(modelId, { value: localValue });
    }
  }, [modelId, continuousUpdate, localValue, value, sendUpdate]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === "Enter") {
        // Send submit event
        sendCustom(modelId, { event: "submit" });
        // Also ensure value is synced
        if (!continuousUpdate && localValue !== value) {
          sendUpdate(modelId, { value: localValue });
        }
      }
    },
    [modelId, continuousUpdate, localValue, value, sendUpdate, sendCustom],
  );

  return (
    <div
      className={cn("flex items-center gap-3", className)}
      data-widget-id={modelId}
      data-widget-type="Password"
    >
      {description && <Label className="shrink-0 text-sm">{description}</Label>}
      <Input
        type="password"
        value={localValue}
        placeholder={placeholder}
        disabled={disabled}
        onChange={handleChange}
        onBlur={handleBlur}
        onKeyDown={handleKeyDown}
        className="flex-1"
      />
    </div>
  );
}

export default PasswordWidget;
