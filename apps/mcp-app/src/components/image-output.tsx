interface ImageOutputProps {
  data: string;
  mediaType: string;
  alt?: string;
}

export function ImageOutput({ data, mediaType, alt = "Output image" }: ImageOutputProps) {
  if (!data) return null;
  const src =
    data.startsWith("data:") ||
    data.startsWith("http://") ||
    data.startsWith("https://") ||
    data.startsWith("/")
      ? data
      : `data:${mediaType};base64,${data}`;
  return <img className="image-output" src={src} alt={alt} />;
}
