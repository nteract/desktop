/**
 * Valid widget - displays a validation indicator (checkmark or X).
 *
 * Maps to ipywidgets ValidModel.
 */

import { CheckIcon, XIcon } from "lucide-react";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue } from "../widget-store-context";

export function ValidWidget({ modelId, className }: WidgetComponentProps) {
  const value = useWidgetModelValue<boolean>(modelId, "value") ?? false;
  const readout = useWidgetModelValue<string>(modelId, "readout") ?? "";
  const description = useWidgetModelValue<string>(modelId, "description");

  return (
    <div
      className={cn("inline-flex items-center gap-2", className)}
      data-widget-id={modelId}
      data-widget-type="Valid"
    >
      {description && <Label className="shrink-0 text-sm">{description}</Label>}
      {value ? (
        <CheckIcon className="size-4 text-green-500" />
      ) : (
        <XIcon className="size-4 text-red-500" />
      )}
      {!value && readout && <span className="text-sm text-muted-foreground">{readout}</span>}
    </div>
  );
}

export default ValidWidget;
