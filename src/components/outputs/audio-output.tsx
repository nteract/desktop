import { cn } from "@/lib/utils";

interface AudioOutputProps {
  /**
   * Audio data — blob URL, data URL, or base64-encoded string
   */
  data: string;
  /**
   * The media type of the audio (e.g. "audio/wav", "audio/mpeg")
   */
  mediaType?: string;
  /**
   * Additional CSS classes
   */
  className?: string;
}

/**
 * Renders an audio player for notebook outputs.
 * Handles blob URLs from the blob store, data URLs, and base64-encoded audio.
 */
export function AudioOutput({
  data,
  mediaType = "audio/wav",
  className = "",
}: AudioOutputProps) {
  if (!data) return null;

  const src =
    data.startsWith("data:") ||
    data.startsWith("http://") ||
    data.startsWith("https://") ||
    data.startsWith("/")
      ? data
      : `data:${mediaType};base64,${data}`;

  return (
    <div data-slot="audio-output" className={cn("py-2", className)}>
      {/* biome-ignore lint/a11y/useMediaCaption: kernel audio outputs don't include captions */}
      <audio src={src} controls preload="metadata" />
    </div>
  );
}
