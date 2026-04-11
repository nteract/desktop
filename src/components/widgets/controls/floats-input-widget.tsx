/**
 * FloatsInput widget - multi-value float tag input.
 *
 * Maps to ipywidgets FloatsInputModel.
 */

import { XIcon } from "lucide-react";
import { useCallback, useState } from "react";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

export function FloatsInputWidget({ modelId, className }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();

  // Subscribe to individual state keys
  const value = useWidgetModelValue<number[]>(modelId, "value") ?? [];
  const allowDuplicates = useWidgetModelValue<boolean>(modelId, "allow_duplicates") ?? true;
  const placeholder = useWidgetModelValue<string>(modelId, "placeholder") ?? "Add number...";
  const description = useWidgetModelValue<string>(modelId, "description");
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;
  const min = useWidgetModelValue<number>(modelId, "min");
  const max = useWidgetModelValue<number>(modelId, "max");

  const [inputValue, setInputValue] = useState("");

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === "Enter" && inputValue.trim()) {
        e.preventDefault();
        const parsed = parseFloat(inputValue.trim());

        // Validate float
        if (Number.isNaN(parsed)) return;

        // Check bounds
        if (min != null && parsed < min) return;
        if (max != null && parsed > max) return;

        // Check for duplicates
        if (!allowDuplicates && value.includes(parsed)) return;

        sendUpdate(modelId, { value: [...value, parsed] });
        setInputValue("");
      } else if (e.key === "Backspace" && !inputValue && value.length > 0) {
        sendUpdate(modelId, { value: value.slice(0, -1) });
      }
    },
    [modelId, inputValue, value, allowDuplicates, min, max, sendUpdate],
  );

  const handleRemoveTag = useCallback(
    (indexToRemove: number) => {
      sendUpdate(modelId, {
        value: value.filter((_, idx) => idx !== indexToRemove),
      });
    },
    [modelId, value, sendUpdate],
  );

  return (
    <div
      className={cn("flex items-start gap-3", className)}
      data-widget-id={modelId}
      data-widget-type="FloatsInput"
    >
      {description && <Label className="shrink-0 pt-2 text-sm">{description}</Label>}
      <div
        className={cn(
          "flex min-h-10 flex-wrap items-center gap-1.5 rounded-md border border-input bg-background px-3 py-2",
          disabled && "cursor-not-allowed opacity-50",
        )}
      >
        {value.map((num, idx) => (
          <Badge key={`${num}-${idx}`} variant="secondary" className="gap-1 pr-1">
            {num}
            {!disabled && (
              <button
                type="button"
                onClick={() => handleRemoveTag(idx)}
                className="rounded-full p-0.5 hover:bg-muted"
              >
                <XIcon className="size-3" />
              </button>
            )}
          </Badge>
        ))}
        <Input
          type="text"
          inputMode="decimal"
          value={inputValue}
          onChange={(e) => setInputValue(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder={value.length === 0 ? placeholder : ""}
          disabled={disabled}
          className="h-6 min-w-20 flex-1 border-0 p-0 shadow-none focus-visible:ring-0"
        />
      </div>
    </div>
  );
}

export default FloatsInputWidget;
