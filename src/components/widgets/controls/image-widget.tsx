/**
 * Image widget - displays images from the kernel.
 *
 * Maps to ipywidgets ImageModel.
 */

import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import { buildMediaSrc } from "../buffer-utils";
import { toCssLength } from "../css-length";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue } from "../widget-store-context";

export function ImageWidget({ modelId, className }: WidgetComponentProps) {
  const value = useWidgetModelValue<string | ArrayBuffer | DataView>(modelId, "value");
  const format = useWidgetModelValue<string>(modelId, "format") ?? "png";
  const width = useWidgetModelValue<string | number>(modelId, "width");
  const height = useWidgetModelValue<string | number>(modelId, "height");
  const description = useWidgetModelValue<string>(modelId, "description");

  const src = buildMediaSrc(value, "image", format);

  if (!src) {
    return null;
  }

  // ipywidgets Image declares width/height as CUnicode, so `Image(width=64)`
  // arrives as the bare string "64" — not a valid CSS length. The canonical
  // ipywidgets JS sets these as HTML attributes (where bare numerics are
  // pixels); we apply the same coercion so the image has real size instead
  // of falling back to its 1x1 intrinsic dimensions.
  const cssWidth = toCssLength(width);
  const cssHeight = toCssLength(height);

  const style: React.CSSProperties = {};
  if (cssWidth) style.width = cssWidth;
  if (cssHeight) style.height = cssHeight;

  return (
    <div
      className={cn("inline-flex items-start gap-3", className)}
      data-widget-id={modelId}
      data-widget-type="Image"
    >
      {description && <Label className="shrink-0 pt-1 text-sm">{description}</Label>}
      <img
        src={src}
        alt={description || "Widget image"}
        className="block max-w-full h-auto"
        style={{ objectFit: "contain", ...style }}
      />
    </div>
  );
}

export default ImageWidget;
