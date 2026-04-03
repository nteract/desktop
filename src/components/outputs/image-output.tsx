import { cn } from "@/lib/utils";

interface ImageOutputProps {
  /**
   * Image data - can be base64 encoded string, data URL, or regular URL
   */
  data: string;
  /**
   * The media type of the image
   */
  mediaType?: string;
  /**
   * Alt text for accessibility
   */
  alt?: string;
  /**
   * Optional width constraint
   */
  width?: number;
  /**
   * Optional height constraint
   */
  height?: number;
  /**
   * Additional CSS classes
   */
  className?: string;
}

/**
 * ImageOutput component for rendering images in notebook outputs
 *
 * Handles base64-encoded image data from Jupyter kernels as well as
 * regular image URLs. Supports any browser-renderable image format.
 */
export function ImageOutput({
  data,
  mediaType = "image/png",
  alt = "Output image",
  width,
  height,
  className = "",
}: ImageOutputProps) {
  if (!data) {
    return null;
  }

  // Determine the image source:
  // - If already a data URL or regular URL, use as-is
  // - Otherwise, assume base64 and construct data URL
  const src =
    data.startsWith("data:") ||
    data.startsWith("http://") ||
    data.startsWith("https://") ||
    data.startsWith("/")
      ? data
      : `data:${mediaType};base64,${data}`;

  const sizeProps: { width?: number; height?: number } = {};
  if (width) sizeProps.width = width;
  if (height) sizeProps.height = height;

  return (
    <div data-slot="image-output" className={cn("not-prose py-2", className)}>
      <img
        src={src}
        alt={alt}
        className="block max-w-full h-auto"
        style={{ objectFit: "contain" }}
        {...sizeProps}
      />
    </div>
  );
}
