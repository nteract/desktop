/**
 * Combobox widget - text input with autocomplete suggestions.
 *
 * Maps to ipywidgets ComboboxModel.
 *
 * Uses native HTML <input> with <datalist> instead of Radix UI Popover
 * because Radix uses Portal which doesn't work in sandboxed iframes
 * (no allow-same-origin). See: https://github.com/runtimed/runt/issues/62
 */

import { useCallback, useEffect, useId, useState } from "react";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

export function ComboboxWidget({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate, sendCustom } = useWidgetStoreRequired();
  const datalistId = useId();

  // Subscribe to individual state keys
  const value = useWidgetModelValue<string>(modelId, "value") ?? "";
  const options = useWidgetModelValue<string[]>(modelId, "options") ?? [];
  const placeholder = useWidgetModelValue<string>(modelId, "placeholder") ?? "Select or type...";
  const description = useWidgetModelValue<string>(modelId, "description");
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;
  const ensureOption = useWidgetModelValue<boolean>(modelId, "ensure_option") ?? false;
  const continuousUpdate = useWidgetModelValue<boolean>(modelId, "continuous_update") ?? true;

  const [inputValue, setInputValue] = useState(value);

  // Sync input value when value changes from kernel
  useEffect(() => {
    setInputValue(value);
  }, [value]);

  const handleChange = useCallback(
    (event: React.ChangeEvent<HTMLInputElement>) => {
      const newValue = event.target.value;
      setInputValue(newValue);
      if (continuousUpdate) {
        // Only update if not enforcing option or value is in options
        if (!ensureOption || options.includes(newValue)) {
          sendUpdate(modelId, { value: newValue });
        }
      }
    },
    [modelId, continuousUpdate, ensureOption, options, sendUpdate],
  );

  const handleBlur = useCallback(() => {
    if (!continuousUpdate) {
      if (!ensureOption || options.includes(inputValue)) {
        sendUpdate(modelId, { value: inputValue });
      } else {
        // Reset to last valid value
        setInputValue(value);
      }
    }
    sendCustom(modelId, { event: "submit" });
  }, [modelId, continuousUpdate, ensureOption, options, inputValue, value, sendUpdate, sendCustom]);

  return (
    <div
      className={cn("flex items-center gap-3", className)}
      data-widget-id={modelId}
      data-widget-type="Combobox"
    >
      {description && <Label className="shrink-0 text-sm">{description}</Label>}
      <input
        type="text"
        list={datalistId}
        value={inputValue}
        onChange={handleChange}
        onBlur={handleBlur}
        placeholder={placeholder}
        disabled={disabled}
        className={cn(
          // Base styles matching shadcn input
          "h-9 w-48 rounded-md border border-input px-3 py-2 text-sm",
          "bg-transparent shadow-xs transition-[color,box-shadow] outline-none",
          // Dark mode background
          "dark:bg-input/30 dark:hover:bg-input/50",
          // Placeholder
          "placeholder:text-muted-foreground",
          // Focus styles
          "focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50",
          // Disabled state
          "disabled:cursor-not-allowed disabled:opacity-50",
        )}
      />
      <datalist id={datalistId}>
        {options.map((option, idx) => (
          <option key={idx} value={option} />
        ))}
      </datalist>
    </div>
  );
}

export default ComboboxWidget;
