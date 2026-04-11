/**
 * SelectMultiple widget - renders a multi-select listbox.
 *
 * Maps to ipywidgets SelectMultipleModel.
 */

import { CheckIcon } from "lucide-react";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

export function SelectMultipleWidget({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();

  // Subscribe to individual state keys
  const options = useWidgetModelValue<string[]>(modelId, "_options_labels") ?? [];
  const selectedIndices = useWidgetModelValue<number[]>(modelId, "index") ?? [];
  const description = useWidgetModelValue<string>(modelId, "description");
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;
  const rows = useWidgetModelValue<number>(modelId, "rows") ?? 5;

  const handleToggle = (idx: number) => {
    if (disabled) return;

    const newIndices = selectedIndices.includes(idx)
      ? selectedIndices.filter((i) => i !== idx)
      : [...selectedIndices, idx].sort((a, b) => a - b);

    sendUpdate(modelId, { index: newIndices });
  };

  // Calculate height based on rows
  const itemHeight = 32; // approximate height per item in px
  const maxHeight = rows * itemHeight;

  return (
    <div
      className={cn("flex items-start gap-3", className)}
      data-widget-id={modelId}
      data-widget-type="SelectMultiple"
    >
      {description && <Label className="shrink-0 text-sm pt-1">{description}</Label>}
      <div
        role="listbox"
        aria-multiselectable="true"
        aria-disabled={disabled}
        className={cn(
          "w-48 overflow-y-auto rounded-md border border-input bg-background shadow-xs",
          disabled && "opacity-50 cursor-not-allowed",
        )}
        style={{ maxHeight }}
      >
        {options.map((option, idx) => {
          const isSelected = selectedIndices.includes(idx);
          return (
            <div
              key={idx}
              role="option"
              aria-selected={isSelected}
              onClick={() => handleToggle(idx)}
              className={cn(
                "flex items-center gap-2 px-3 py-1.5 text-sm cursor-pointer select-none",
                "hover:bg-accent hover:text-accent-foreground",
                isSelected && "bg-accent/50",
                disabled && "pointer-events-none",
              )}
            >
              <span className="flex size-4 items-center justify-center">
                {isSelected && <CheckIcon className="size-3" />}
              </span>
              <span>{option}</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

export default SelectMultipleWidget;
