/**
 * ColorsInput widget - multi-value color input.
 *
 * Maps to ipywidgets ColorsInputModel.
 */

import { PlusIcon, XIcon } from "lucide-react";
import { useCallback, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

export function ColorsInputWidget({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();
  const colorInputRef = useRef<HTMLInputElement>(null);

  // Subscribe to individual state keys
  const value = useWidgetModelValue<string[]>(modelId, "value") ?? [];
  const allowedTags = useWidgetModelValue<string[]>(modelId, "allowed_tags");
  const allowDuplicates = useWidgetModelValue<boolean>(modelId, "allow_duplicates") ?? true;
  const description = useWidgetModelValue<string>(modelId, "description");
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;

  const [editingIndex, setEditingIndex] = useState<number | null>(null);

  const handleAddColor = useCallback(() => {
    colorInputRef.current?.click();
  }, []);

  const handleColorChange = useCallback(
    (e: React.ChangeEvent<HTMLInputElement>) => {
      const newColor = e.target.value;

      // Check if color is allowed
      if (allowedTags && allowedTags.length > 0 && !allowedTags.includes(newColor)) {
        return;
      }

      // Check for duplicates
      if (!allowDuplicates && value.includes(newColor)) {
        return;
      }

      if (editingIndex !== null) {
        // Update existing color
        const newValue = [...value];
        newValue[editingIndex] = newColor;
        sendUpdate(modelId, { value: newValue });
        setEditingIndex(null);
      } else {
        // Add new color
        sendUpdate(modelId, { value: [...value, newColor] });
      }
    },
    [modelId, value, allowedTags, allowDuplicates, editingIndex, sendUpdate],
  );

  const handleRemoveColor = useCallback(
    (indexToRemove: number) => {
      sendUpdate(modelId, {
        value: value.filter((_, idx) => idx !== indexToRemove),
      });
    },
    [modelId, value, sendUpdate],
  );

  const handleEditColor = useCallback((idx: number) => {
    setEditingIndex(idx);
    // Trigger color picker with slight delay to allow state update
    setTimeout(() => {
      colorInputRef.current?.click();
    }, 0);
  }, []);

  return (
    <div
      className={cn("flex items-start gap-3", className)}
      data-widget-id={modelId}
      data-widget-type="ColorsInput"
    >
      {description && <Label className="shrink-0 pt-2 text-sm">{description}</Label>}
      <div
        className={cn(
          "flex flex-wrap items-center gap-2",
          disabled && "cursor-not-allowed opacity-50",
        )}
      >
        {value.map((color, idx) => (
          <div key={`${color}-${idx}`} className="group relative flex items-center">
            <button
              type="button"
              onClick={() => handleEditColor(idx)}
              disabled={disabled}
              className="size-8 rounded-md border border-input shadow-sm transition-transform hover:scale-105"
              style={{ backgroundColor: color }}
              title={color}
            />
            {!disabled && (
              <button
                type="button"
                onClick={() => handleRemoveColor(idx)}
                className="absolute -right-1 -top-1 rounded-full bg-background p-0.5 opacity-0 shadow-sm transition-opacity group-hover:opacity-100"
              >
                <XIcon className="size-3" />
              </button>
            )}
          </div>
        ))}
        <input
          ref={colorInputRef}
          type="color"
          value={editingIndex !== null ? value[editingIndex] : "#000000"}
          onChange={handleColorChange}
          disabled={disabled}
          className="sr-only"
        />
        <Button
          variant="outline"
          size="icon"
          className="size-8"
          onClick={handleAddColor}
          disabled={disabled}
        >
          <PlusIcon className="size-4" />
        </Button>
      </div>
    </div>
  );
}

export default ColorsInputWidget;
