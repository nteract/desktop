/**
 * Video widget - plays video from the kernel.
 *
 * Maps to ipywidgets VideoModel.
 */

import { useMemo } from "react";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import { buildMediaSrc } from "../buffer-utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue } from "../widget-store-context";

export function VideoWidget({ modelId, className }: WidgetComponentProps) {
  const value = useWidgetModelValue<string | ArrayBuffer>(modelId, "value");
  const format = useWidgetModelValue<string>(modelId, "format") ?? "mp4";
  const width = useWidgetModelValue<string>(modelId, "width") ?? "";
  const height = useWidgetModelValue<string>(modelId, "height") ?? "";
  const autoplay = useWidgetModelValue<boolean>(modelId, "autoplay") ?? true;
  const loop = useWidgetModelValue<boolean>(modelId, "loop") ?? true;
  const controls = useWidgetModelValue<boolean>(modelId, "controls") ?? true;
  const description = useWidgetModelValue<string>(modelId, "description");

  const src = useMemo(() => buildMediaSrc(value, "video", format), [value, format]);

  if (!value) {
    return null;
  }

  const style: React.CSSProperties = {};
  if (width) style.width = width;
  if (height) style.height = height;

  return (
    <div
      className={cn("inline-flex items-start gap-3", className)}
      data-widget-id={modelId}
      data-widget-type="Video"
    >
      {description && <Label className="shrink-0 pt-1 text-sm">{description}</Label>}
      <video
        src={src}
        autoPlay={autoplay}
        loop={loop}
        controls={controls}
        style={style}
        className="max-w-full"
      />
    </div>
  );
}

export default VideoWidget;
