/**
 * Textarea widget - renders a multi-line text input.
 *
 * Maps to ipywidgets TextareaModel.
 */

import { useCallback, useEffect, useState } from "react";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

export function TextareaWidget({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();

  // Subscribe to individual state keys
  const value = useWidgetModelValue<string>(modelId, "value") ?? "";
  const description = useWidgetModelValue<string>(modelId, "description");
  const placeholder = useWidgetModelValue<string>(modelId, "placeholder") ?? "";
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;
  const rows = useWidgetModelValue<number>(modelId, "rows") ?? 4;
  const continuousUpdate = useWidgetModelValue<boolean>(modelId, "continuous_update") ?? true;

  // Local state for non-continuous updates
  const [localValue, setLocalValue] = useState(value);

  // Sync local state when value changes from kernel
  useEffect(() => {
    setLocalValue(value);
  }, [value]);

  const handleChange = useCallback(
    (e: React.ChangeEvent<HTMLTextAreaElement>) => {
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

  return (
    <div
      className={cn("flex flex-col gap-2", className)}
      data-widget-id={modelId}
      data-widget-type="Textarea"
    >
      {description && <Label className="text-sm">{description}</Label>}
      <Textarea
        value={localValue}
        placeholder={placeholder}
        disabled={disabled}
        rows={rows}
        onChange={handleChange}
        onBlur={handleBlur}
        className="resize-y"
      />
    </div>
  );
}

export default TextareaWidget;
