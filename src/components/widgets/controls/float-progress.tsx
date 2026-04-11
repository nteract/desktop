/**
 * FloatProgress widget - renders a floating point progress bar.
 *
 * Maps to ipywidgets FloatProgressModel.
 */

import { Label } from "@/components/ui/label";
import { Progress } from "@/components/ui/progress";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue } from "../widget-store-context";

export function FloatProgress({ modelId, className }: WidgetComponentProps) {
  // Subscribe to individual state keys
  const value = useWidgetModelValue<number>(modelId, "value") ?? 0;
  const min = useWidgetModelValue<number>(modelId, "min") ?? 0;
  const max = useWidgetModelValue<number>(modelId, "max") ?? 100;
  const description = useWidgetModelValue<string>(modelId, "description");
  const barStyle =
    useWidgetModelValue<"success" | "info" | "warning" | "danger" | "">(modelId, "bar_style") ?? "";
  const orientation =
    useWidgetModelValue<"horizontal" | "vertical">(modelId, "orientation") ?? "horizontal";

  // Calculate percentage
  const range = max - min;
  const percentage = range > 0 ? ((value - min) / range) * 100 : 0;

  // Map bar_style to Tailwind classes
  const barStyleClasses: Record<string, string> = {
    success: "[&>[data-slot=progress-indicator]]:bg-green-500",
    info: "[&>[data-slot=progress-indicator]]:bg-blue-500",
    warning: "[&>[data-slot=progress-indicator]]:bg-yellow-500",
    danger: "[&>[data-slot=progress-indicator]]:bg-red-500",
  };

  const isVertical = orientation === "vertical";

  return (
    <div
      className={cn(
        "flex gap-3",
        isVertical ? "flex-col items-center" : "flex-1 items-center",
        className,
      )}
      data-widget-id={modelId}
      data-widget-type="FloatProgress"
    >
      {description && <Label className="shrink-0 text-sm">{description}</Label>}
      <Progress
        value={percentage}
        className={cn(
          isVertical ? "h-32 w-2" : "flex-1 min-w-24",
          barStyle && barStyleClasses[barStyle],
        )}
      />
    </div>
  );
}

export default FloatProgress;
