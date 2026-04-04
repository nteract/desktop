import { useEffect, useRef, useState } from "react";
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
 *
 * When the src changes (e.g., interactive widget updates), the new image
 * is preloaded in the background. The old image stays visible until the
 * new one is ready, then swaps in with a brief crossfade.
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
  const targetSrc =
    data.startsWith("data:") ||
    data.startsWith("http://") ||
    data.startsWith("https://") ||
    data.startsWith("/")
      ? data
      : `data:${mediaType};base64,${data}`;

  return (
    <div data-slot="image-output" className={cn("not-prose py-2", className)}>
      <PreloadedImage
        src={targetSrc}
        alt={alt}
        width={width}
        height={height}
      />
    </div>
  );
}

/**
 * Image element that preloads new sources before displaying them.
 * Keeps the previous image visible during loading to avoid flicker.
 */
function PreloadedImage({
  src,
  alt,
  width,
  height,
}: {
  src: string;
  alt: string;
  width?: number;
  height?: number;
}) {
  // The src currently being displayed
  const [displaySrc, setDisplaySrc] = useState(src);
  const preloadRef = useRef<HTMLImageElement | null>(null);

  useEffect(() => {
    if (src === displaySrc) return;

    // Cancel any previous preload
    if (preloadRef.current) {
      preloadRef.current.onload = null;
      preloadRef.current.onerror = null;
    }

    const img = new Image();
    preloadRef.current = img;

    img.onload = () => {
      // New image is cached by the browser — swap instantly
      setDisplaySrc(src);
      preloadRef.current = null;
    };
    img.onerror = () => {
      // Preload failed — show new src anyway (will show broken image)
      setDisplaySrc(src);
      preloadRef.current = null;
    };
    img.src = src;

    return () => {
      img.onload = null;
      img.onerror = null;
    };
  }, [src, displaySrc]);

  const sizeProps: { width?: number; height?: number } = {};
  if (width) sizeProps.width = width;
  if (height) sizeProps.height = height;

  return (
    <img
      src={displaySrc}
      alt={alt}
      className="block max-w-full h-auto"
      style={{ objectFit: "contain" }}
      {...sizeProps}
    />
  );
}
