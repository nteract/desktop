/**
 * Select widget - renders a single-selection listbox.
 *
 * Maps to ipywidgets SelectModel.
 */

import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

export function SelectWidget({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();

  // Subscribe to individual state keys
  const options = useWidgetModelValue<string[]>(modelId, "_options_labels") ?? [];
  const selectedIndex = useWidgetModelValue<number | null>(modelId, "index");
  const description = useWidgetModelValue<string>(modelId, "description");
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;
  const rows = useWidgetModelValue<number>(modelId, "rows") ?? 5;

  const handleSelect = (idx: number) => {
    if (disabled) return;
    sendUpdate(modelId, { index: idx });
  };

  // Calculate height based on rows
  const itemHeight = 32; // approximate height per item in px
  const maxHeight = rows * itemHeight;

  return (
    <div
      className={cn("flex items-start gap-3", className)}
      data-widget-id={modelId}
      data-widget-type="Select"
    >
      {description && <Label className="shrink-0 pt-1 text-sm">{description}</Label>}
      <div
        role="listbox"
        aria-disabled={disabled}
        className={cn(
          "w-48 overflow-y-auto rounded-md border border-input bg-background shadow-xs",
          disabled && "cursor-not-allowed opacity-50",
        )}
        style={{ maxHeight }}
      >
        {options.map((option, idx) => {
          const isSelected = selectedIndex === idx;
          return (
            <div
              key={idx}
              role="option"
              aria-selected={isSelected}
              onClick={() => handleSelect(idx)}
              className={cn(
                "flex cursor-pointer select-none items-center px-3 py-1.5 text-sm",
                "hover:bg-accent hover:text-accent-foreground",
                isSelected && "bg-accent text-accent-foreground",
                disabled && "pointer-events-none",
              )}
            >
              <span>{option}</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

export default SelectWidget;
