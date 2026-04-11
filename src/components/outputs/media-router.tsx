import { lazy, type ReactNode, Suspense } from "react";
import { getRenderer } from "@/lib/renderer-registry";
import { isSafeForMainDom } from "./safe-mime-types";
import { useMediaContext } from "./media-provider";

// Lazy load built-in output components for better bundle splitting
const AnsiOutput = lazy(() => import("./ansi-output").then((m) => ({ default: m.AnsiOutput })));
const MarkdownOutput = lazy(() =>
  import("./markdown-output").then((m) => ({ default: m.MarkdownOutput })),
);
const HtmlOutput = lazy(() => import("./html-output").then((m) => ({ default: m.HtmlOutput })));
const ImageOutput = lazy(() => import("./image-output").then((m) => ({ default: m.ImageOutput })));
const SvgOutput = lazy(() => import("./svg-output").then((m) => ({ default: m.SvgOutput })));
const JsonOutput = lazy(() => import("./json-output").then((m) => ({ default: m.JsonOutput })));

const AudioOutput = lazy(() => import("./audio-output").then((m) => ({ default: m.AudioOutput })));
const VideoOutput = lazy(() => import("./video-output").then((m) => ({ default: m.VideoOutput })));
const PdfOutput = lazy(() => import("./pdf-output").then((m) => ({ default: m.PdfOutput })));
const JavaScriptOutput = lazy(() =>
  import("./javascript-output").then((m) => ({
    default: m.JavaScriptOutput,
  })),
);

/**
 * Check if the current window is inside an iframe
 */
function isInIframe(): boolean {
  if (typeof window === "undefined") return false;
  try {
    return window.self !== window.top;
  } catch {
    // If we can't access window.top due to cross-origin restrictions,
    // we're definitely in an iframe
    return true;
  }
}

/**
 * Default MIME type priority order for rendering.
 * Higher priority types are preferred when multiple are available.
 * Platforms can override this with the `priority` prop.
 */
export const DEFAULT_PRIORITY = [
  // Rich formats first
  "application/vnd.jupyter.widget-view+json",
  "application/vnd.plotly.v1+json",
  "application/vnd.vegalite.v6+json",
  "application/vnd.vegalite.v6.json",
  "application/vnd.vegalite.v5+json",
  "application/vnd.vegalite.v5.json",
  "application/vnd.vegalite.v4+json",
  "application/vnd.vegalite.v3+json",
  "application/vnd.vega.v6+json",
  "application/vnd.vega.v6.json",
  "application/vnd.vega.v5+json",
  "application/vnd.vega.v5.json",
  "application/vnd.vega.v4+json",
  "application/geo+json",
  // HTML, PDF, markdown, and LaTeX
  "text/html",
  "application/pdf",
  "text/markdown",
  "text/latex",
  "application/javascript",
  // Images
  "image/svg+xml",
  "image/png",
  "image/jpeg",
  "image/gif",
  "image/webp",
  "image/bmp",
  // Audio
  "audio/wav",
  "audio/mpeg",
  "audio/ogg",
  "audio/flac",
  "audio/webm",
  // Video
  "video/mp4",
  "video/webm",
  "video/ogg",
  // Structured data
  "application/json",
  // Plain text (fallback)
  "text/plain",
] as const;

type MimeType = (typeof DEFAULT_PRIORITY)[number] | string;

interface MediaData {
  [mimeType: string]: unknown;
}

interface MediaMetadata {
  [mimeType: string]:
    | {
        width?: number;
        height?: number;
        [key: string]: unknown;
      }
    | undefined;
}

/**
 * Props passed to custom renderer functions.
 */
export interface RendererProps {
  data: unknown;
  metadata: Record<string, unknown>;
  mimeType: string;
  className?: string;
}

/**
 * Custom renderer function type.
 */
export type CustomRenderer = (props: RendererProps) => ReactNode;

interface MediaRouterProps {
  /**
   * Output data object mapping MIME types to content.
   * e.g., { "text/plain": "Hello", "text/html": "<b>Hello</b>" }
   */
  data: MediaData;
  /**
   * Output metadata object mapping MIME types to their metadata.
   * e.g., { "image/png": { width: 400, height: 300 } }
   */
  metadata?: MediaMetadata;
  /**
   * Custom MIME type priority order. Types listed first are preferred.
   * Defaults to DEFAULT_PRIORITY. Your custom types should come first,
   * followed by spreading DEFAULT_PRIORITY for fallback.
   *
   * @example
   * ```tsx
   * priority={["application/vnd.plotly.v1+json", ...DEFAULT_PRIORITY]}
   * ```
   */
  priority?: readonly string[];
  /**
   * Custom renderers keyed by MIME type. Use this to handle MIME types
   * not supported by the built-in renderers, or to override built-ins.
   *
   * @example
   * ```tsx
   * renderers={{
   *   "application/vnd.plotly.v1+json": ({ data }) => <PlotlyChart data={data} />,
   *   "application/geo+json": ({ data }) => <GeoJsonMap data={data} />,
   * }}
   * ```
   */
  renderers?: Record<string, CustomRenderer>;
  /**
   * Custom fallback component when no supported MIME type is found.
   */
  fallback?: ReactNode;
  /**
   * Loading component shown while lazy-loading output components.
   */
  loading?: ReactNode;
  /**
   * Additional CSS classes passed to the rendered output.
   */
  className?: string;
}

/**
 * Select the best MIME type from available data based on priority.
 */
function selectMimeType(data: MediaData, priority: readonly string[]): MimeType | null {
  const availableTypes = Object.keys(data);

  // Check priority list first
  for (const mimeType of priority) {
    if (availableTypes.includes(mimeType) && data[mimeType] != null) {
      return mimeType;
    }
  }

  // Fall back to first available type
  const firstAvailable = availableTypes.find((type) => data[type] != null);
  return firstAvailable || null;
}

/**
 * Default loading spinner
 */
function DefaultLoading() {
  return (
    <div className="flex items-center justify-center py-4 text-gray-400">
      <svg
        className="h-5 w-5 animate-spin"
        xmlns="http://www.w3.org/2000/svg"
        fill="none"
        viewBox="0 0 24 24"
      >
        <circle
          className="opacity-25"
          cx="12"
          cy="12"
          r="10"
          stroke="currentColor"
          strokeWidth="4"
        />
        <path
          className="opacity-75"
          fill="currentColor"
          d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"
        />
      </svg>
    </div>
  );
}

/**
 * MediaRouter component for rendering Jupyter outputs based on MIME type.
 *
 * Automatically selects the best available renderer for the output data,
 * following Jupyter's MIME type priority conventions. Supports custom
 * renderers and priority ordering for platform-specific MIME types.
 *
 * @example
 * ```tsx
 * // Basic usage
 * <MediaRouter
 *   data={{
 *     "text/plain": "Hello, World!",
 *     "text/html": "<b>Hello, World!</b>"
 *   }}
 * />
 *
 * // With custom renderers
 * <MediaRouter
 *   data={output.data}
 *   metadata={output.metadata}
 *   priority={["application/vnd.plotly.v1+json", ...DEFAULT_PRIORITY]}
 *   renderers={{
 *     "application/vnd.plotly.v1+json": ({ data }) => <PlotlyChart data={data} />,
 *   }}
 * />
 * ```
 */
export function MediaRouter({
  data,
  metadata = {},
  priority: priorityProp,
  renderers: renderersProp,
  fallback,
  loading,
  className = "",
}: MediaRouterProps) {
  const ctx = useMediaContext();

  // Props override context, context overrides built-in defaults
  const priority = priorityProp ?? ctx?.priority ?? DEFAULT_PRIORITY;
  const renderers = renderersProp ?? ctx?.renderers ?? {};

  const mimeType = selectMimeType(data, priority);

  if (!mimeType) {
    return (
      <div data-slot="media-router">
        {fallback ? (
          fallback
        ) : (
          <div className="py-2 text-sm text-gray-500">No displayable output</div>
        )}
      </div>
    );
  }

  const content = data[mimeType];
  const mimeMetadata = (metadata[mimeType] || {}) as Record<string, unknown>;
  const loadingComponent = loading || <DefaultLoading />;

  // Check for custom renderer first
  if (renderers[mimeType]) {
    const customRenderer = renderers[mimeType];
    return (
      <div data-slot="media-router" data-mime-type={mimeType}>
        <Suspense fallback={loadingComponent}>
          {customRenderer({
            data: content,
            metadata: mimeMetadata,
            mimeType,
            className,
          })}
        </Suspense>
      </div>
    );
  }

  const renderBuiltIn = () => {
    // ISOLATION GUARD: Only types in the safe-list can render in the main DOM.
    // Everything else requires iframe isolation for security.
    const needsIsolation = !isSafeForMainDom(mimeType);

    if (needsIsolation && !isInIframe()) {
      if (process.env.NODE_ENV === "development") {
        console.warn(
          `MediaRouter: "${mimeType}" requires iframe isolation. ` +
            `Use OutputArea with isolated="auto" or IsolatedFrame directly.`,
        );
      }
      return null;
    }

    // Check the renderer plugin registry (populated by on-demand plugins
    // like plotly, vega, leaflet, markdown). This allows output widgets to
    // render rich MIME types using any plugin already installed in the iframe.
    const PluginRenderer = getRenderer(mimeType);
    if (PluginRenderer) {
      return <PluginRenderer data={content} metadata={mimeMetadata} mimeType={mimeType} />;
    }

    // Text/Markdown (only renders when in iframe)
    if (mimeType === "text/markdown") {
      return <MarkdownOutput content={String(content)} className={className} />;
    }

    // HTML (only renders when in iframe)
    if (mimeType === "text/html") {
      return <HtmlOutput content={String(content)} className={className} />;
    }

    // SVG (only renders when in iframe)
    if (mimeType === "image/svg+xml") {
      return <SvgOutput data={String(content)} className={className} />;
    }

    // Images (not SVG)
    if (mimeType.startsWith("image/")) {
      return (
        <ImageOutput
          data={String(content)}
          mediaType={mimeType}
          width={mimeMetadata.width as number | undefined}
          height={mimeMetadata.height as number | undefined}
          className={className}
        />
      );
    }

    // Audio
    if (mimeType.startsWith("audio/")) {
      return <AudioOutput data={String(content)} mediaType={mimeType} className={className} />;
    }

    // Video
    if (mimeType.startsWith("video/")) {
      return (
        <VideoOutput
          data={String(content)}
          mediaType={mimeType}
          width={mimeMetadata.width as number | undefined}
          height={mimeMetadata.height as number | undefined}
          className={className}
        />
      );
    }

    // PDF
    if (mimeType === "application/pdf") {
      return <PdfOutput data={String(content)} className={className} />;
    }

    // JavaScript (only in iframe)
    if (mimeType === "application/javascript") {
      return <JavaScriptOutput code={String(content)} className={className} />;
    }

    // JSON and structured data (but not custom +json types without a renderer)
    if (mimeType === "application/json") {
      return (
        <JsonOutput
          data={content}
          collapsed={mimeMetadata.collapsed as boolean | number | undefined}
          className={className}
        />
      );
    }

    // Plain text (may contain ANSI)
    if (mimeType === "text/plain") {
      return <AnsiOutput className={className}>{String(content)}</AnsiOutput>;
    }

    // Widget view JSON - when rendered in-DOM without a widget renderer,
    // show a helpful message instead of raw JSON. This typically happens
    // when a widget is displayed inside an Output widget.
    if (mimeType === "application/vnd.jupyter.widget-view+json") {
      return (
        <div className="py-2 px-3 text-sm text-muted-foreground bg-muted/50 rounded border border-border">
          <span className="font-medium">Nested widget</span>
          <span className="mx-1">·</span>
          <span>Widgets inside Output widgets are not yet supported</span>
        </div>
      );
    }

    // Unknown +json types without custom renderer - show as JSON
    if (mimeType.includes("+json")) {
      return (
        <JsonOutput
          data={content}
          collapsed={mimeMetadata.collapsed as boolean | number | undefined}
          className={className}
        />
      );
    }

    // Unknown text/* types — render with a MIME type label for distinction
    if (mimeType.startsWith("text/")) {
      return (
        <div>
          <span className="inline-block mb-1 px-1.5 py-0.5 text-[10px] font-mono text-muted-foreground bg-muted rounded">
            {mimeType}
          </span>
          <AnsiOutput className={className}>{String(content)}</AnsiOutput>
        </div>
      );
    }

    // Fallback: render as plain text
    return <AnsiOutput className={className}>{String(content)}</AnsiOutput>;
  };

  return (
    <div data-slot="media-router" data-mime-type={mimeType}>
      <Suspense fallback={loadingComponent}>{renderBuiltIn()}</Suspense>
    </div>
  );
}

/**
 * Get the selected MIME type for debugging/display purposes.
 */
export function getSelectedMimeType(
  data: MediaData,
  priority: readonly string[] = DEFAULT_PRIORITY,
): string | null {
  return selectMimeType(data, priority);
}
