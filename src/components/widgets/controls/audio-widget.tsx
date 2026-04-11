/**
 * Audio widget - plays audio from the kernel.
 *
 * Maps to ipywidgets AudioModel.
 */

import { useMemo } from "react";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import { buildMediaSrc } from "../buffer-utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue } from "../widget-store-context";

export function AudioWidget({ modelId, className }: WidgetComponentProps) {
  const value = useWidgetModelValue<string | ArrayBuffer>(modelId, "value");
  const format = useWidgetModelValue<string>(modelId, "format") ?? "mp3";
  const autoplay = useWidgetModelValue<boolean>(modelId, "autoplay") ?? true;
  const loop = useWidgetModelValue<boolean>(modelId, "loop") ?? true;
  const controls = useWidgetModelValue<boolean>(modelId, "controls") ?? true;
  const description = useWidgetModelValue<string>(modelId, "description");

  const src = useMemo(() => buildMediaSrc(value, "audio", format), [value, format]);

  if (!value) {
    return null;
  }

  return (
    <div
      className={cn("inline-flex items-start gap-3", className)}
      data-widget-id={modelId}
      data-widget-type="Audio"
    >
      {description && <Label className="shrink-0 pt-1 text-sm">{description}</Label>}
      <audio src={src} autoPlay={autoplay} loop={loop} controls={controls} />
    </div>
  );
}

export default AudioWidget;
