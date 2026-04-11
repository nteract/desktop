/**
 * Play widget - animation control with play/pause/step.
 *
 * Maps to ipywidgets PlayModel.
 */

import { PauseIcon, PlayIcon, SkipBackIcon, SkipForwardIcon } from "lucide-react";
import { useCallback, useEffect, useRef } from "react";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

export function PlayWidget({ modelId, className }: WidgetComponentProps) {
  // sendUpdate is now stable (useCommRouter uses refs internally)
  const { sendUpdate } = useWidgetStoreRequired();
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Subscribe to individual state keys
  const value = useWidgetModelValue<number>(modelId, "value") ?? 0;
  const min = useWidgetModelValue<number>(modelId, "min") ?? 0;
  const max = useWidgetModelValue<number>(modelId, "max") ?? 100;
  const step = useWidgetModelValue<number>(modelId, "step") ?? 1;
  const interval = useWidgetModelValue<number>(modelId, "interval") ?? 100; // ms
  const playing = useWidgetModelValue<boolean>(modelId, "_playing") ?? false;
  const repeat = useWidgetModelValue<boolean>(modelId, "repeat") ?? false;
  const description = useWidgetModelValue<string>(modelId, "description");
  const disabled = useWidgetModelValue<boolean>(modelId, "disabled") ?? false;

  // Ref for animation state values - these change frequently and shouldn't restart the interval.
  // sendUpdate is stable so it doesn't need a ref.
  const valuesRef = useRef({ value, min, max, step, repeat });

  // Keep values ref up-to-date without triggering the main effect
  useEffect(() => {
    valuesRef.current = { value, min, max, step, repeat };
  });

  // Handle animation interval - only restart when play state or interval timing changes
  useEffect(() => {
    if (playing && !disabled) {
      intervalRef.current = setInterval(() => {
        const { value, min, max, step, repeat } = valuesRef.current;
        const nextValue = value + step > max ? (repeat ? min : max) : value + step;
        const shouldStop = value + step > max && !repeat;
        sendUpdate(modelId, {
          value: nextValue,
          _playing: !shouldStop,
        });
      }, interval);
    } else {
      if (intervalRef.current) {
        clearInterval(intervalRef.current);
        intervalRef.current = null;
      }
    }

    return () => {
      if (intervalRef.current) {
        clearInterval(intervalRef.current);
      }
    };
  }, [playing, disabled, interval, modelId, sendUpdate]);

  const handlePlayPause = useCallback(() => {
    sendUpdate(modelId, { _playing: !playing });
  }, [modelId, playing, sendUpdate]);

  const handleStepBack = useCallback(() => {
    const { min, step, value } = valuesRef.current;
    const newValue = Math.max(min, value - step);
    sendUpdate(modelId, { value: newValue, _playing: false });
  }, [modelId, sendUpdate]);

  const handleStepForward = useCallback(() => {
    const { max, step, value } = valuesRef.current;
    const newValue = Math.min(max, value + step);
    sendUpdate(modelId, { value: newValue, _playing: false });
  }, [modelId, sendUpdate]);

  return (
    <div
      className={cn("flex items-center gap-2", className)}
      data-widget-id={modelId}
      data-widget-type="Play"
    >
      {description && <Label className="shrink-0 text-sm">{description}</Label>}
      <div className="flex items-center gap-1">
        <Button
          variant="outline"
          size="icon"
          className="size-8"
          onClick={handleStepBack}
          disabled={disabled || value <= min}
        >
          <SkipBackIcon className="size-4" />
        </Button>
        <Button
          variant="outline"
          size="icon"
          className="size-8"
          onClick={handlePlayPause}
          disabled={disabled}
        >
          {playing ? <PauseIcon className="size-4" /> : <PlayIcon className="size-4" />}
        </Button>
        <Button
          variant="outline"
          size="icon"
          className="size-8"
          onClick={handleStepForward}
          disabled={disabled || value >= max}
        >
          <SkipForwardIcon className="size-4" />
        </Button>
      </div>
      <span className="w-12 text-right text-sm tabular-nums text-muted-foreground">{value}</span>
    </div>
  );
}

export default PlayWidget;
