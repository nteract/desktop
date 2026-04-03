import { cn } from "@/lib/utils";

interface PdfOutputProps {
  /**
   * PDF data — blob URL, data URL, or base64-encoded string
   */
  data: string;
  /**
   * Additional CSS classes
   */
  className?: string;
}

/**
 * Renders a PDF viewer for notebook outputs.
 * Uses <embed> for inline viewing with a download link fallback.
 */
export function PdfOutput({ data, className = "" }: PdfOutputProps) {
  if (!data) return null;

  const src =
    data.startsWith("data:") ||
    data.startsWith("http://") ||
    data.startsWith("https://") ||
    data.startsWith("/")
      ? data
      : `data:application/pdf;base64,${data}`;

  return (
    <div data-slot="pdf-output" className={cn("py-2", className)}>
      <embed
        src={src}
        type="application/pdf"
        className="w-full rounded border border-border"
        style={{ minHeight: "400px", height: "600px" }}
      />
      <a
        href={src}
        download="output.pdf"
        className="mt-1 inline-block text-xs text-muted-foreground hover:text-foreground underline"
      >
        Download PDF
      </a>
    </div>
  );
}
