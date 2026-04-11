/**
 * ToggleButtons widget - renders a group of toggle buttons (single selection).
 *
 * Maps to ipywidgets ToggleButtonsModel.
 */

import { Label } from "@/components/ui/label";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

export function ToggleButtonsWidget({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();

  // Subscribe to individual state keys
  const options = useWidgetModelValue<string[]>(modelId, "_options_labels") ?? [];
  const index = useWidgetModelValue<number | null>(modelId, "index");
  const description = useWidgetModelValue<string>(modelId, "description");
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;
  const icons = useWidgetModelValue<string[]>(modelId, "icons") ?? [];
  const tooltips = useWidgetModelValue<string[]>(modelId, "tooltips") ?? [];

  // Convert index to string value for ToggleGroup
  const value = index !== null && index !== undefined && index >= 0 ? String(index) : undefined;

  const handleValueChange = (newValue: string) => {
    if (newValue === "") {
      // Deselection - ToggleButtons typically require a selection
      return;
    }
    const newIndex = parseInt(newValue, 10);
    if (!Number.isNaN(newIndex)) {
      sendUpdate(modelId, { index: newIndex });
    }
  };

  return (
    <div
      className={cn("flex items-center gap-3", className)}
      data-widget-id={modelId}
      data-widget-type="ToggleButtons"
    >
      {description && <Label className="shrink-0 text-sm">{description}</Label>}
      <ToggleGroup
        type="single"
        value={value}
        onValueChange={handleValueChange}
        disabled={disabled}
        variant="outline"
      >
        {options.map((option, idx) => (
          <ToggleGroupItem
            key={idx}
            value={String(idx)}
            title={tooltips[idx] || undefined}
            disabled={disabled}
          >
            {icons[idx] && <span className="mr-1">{icons[idx]}</span>}
            {option}
          </ToggleGroupItem>
        ))}
      </ToggleGroup>
    </div>
  );
}

export default ToggleButtonsWidget;
