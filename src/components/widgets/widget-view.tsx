/**
 * Universal widget view component.
 *
 * Routes widget models to the appropriate renderer:
 * - anywidgets (with _esm field) → AnyWidgetView
 * - Built-in widgets → shadcn-backed components
 * - Unknown widgets → UnsupportedWidget fallback
 */

import { ErrorBoundary } from "@/lib/error-boundary";
import { cn } from "@/lib/utils";
import { WidgetErrorFallback } from "@/lib/widget-error-fallback";
import { AnyWidgetView, isAnyWidget } from "./anywidget-view";
import { useLayoutStyles } from "./use-layout-styles";
import { getWidgetComponent } from "./widget-registry";
import type { WidgetModel } from "./widget-store";
import { useWasWidgetClosed, useWidgetModel } from "./widget-store-context";

// === Props ===

export interface WidgetViewProps {
  /** The model ID (comm_id) of the widget to render */
  modelId: string;
  /** Optional className for the container */
  className?: string;
}

// === Fallback Components ===

function LoadingWidget({ modelId, className }: WidgetViewProps) {
  return (
    <div
      className={cn("text-muted-foreground text-sm animate-pulse", className)}
      data-widget-id={modelId}
      data-widget-loading="true"
    >
      Loading widget...
    </div>
  );
}

interface UnsupportedWidgetProps extends WidgetViewProps {
  model: WidgetModel;
}

function UnsupportedWidget({ model, className }: UnsupportedWidgetProps) {
  return (
    <div
      className={cn(
        "rounded border border-dashed border-muted-foreground/50 p-3 text-sm",
        className,
      )}
      data-widget-id={model.id}
      data-widget-unsupported="true"
    >
      <div className="font-medium text-muted-foreground">Unsupported widget: {model.modelName}</div>
      <div className="text-xs text-muted-foreground/70 mt-1">
        Module: {model.modelModule || "unknown"}
      </div>
    </div>
  );
}

// === Main Component ===

/**
 * Universal widget view that routes to the appropriate renderer.
 *
 * @example
 * ```tsx
 * <WidgetStoreProvider sendMessage={sendToKernel}>
 *   <WidgetView modelId="comm-id-123" />
 * </WidgetStoreProvider>
 * ```
 */
export function WidgetView({ modelId, className }: WidgetViewProps) {
  const model = useWidgetModel(modelId);
  const wasClosed = useWasWidgetClosed(modelId);
  // Get child layout styles (grid_area for positioning within grid containers)
  const { childStyle } = useLayoutStyles(modelId);

  // Model was explicitly closed (e.g., tqdm with leave=False) - render nothing
  if (wasClosed) {
    return null;
  }

  // Model not loaded yet - show loading state
  if (!model) {
    return <LoadingWidget modelId={modelId} className={className} />;
  }

  // Determine the rendered widget content
  let renderedWidget: React.ReactNode;

  // anywidgets have _esm field - render with ESM loader
  if (isAnyWidget(model)) {
    renderedWidget = <AnyWidgetView modelId={modelId} className={className} />;
  } else {
    // Check for built-in widget component
    const WidgetComponent = getWidgetComponent(model.modelName);
    if (WidgetComponent) {
      renderedWidget = <WidgetComponent modelId={modelId} className={className} />;
    } else {
      // No handler found
      renderedWidget = <UnsupportedWidget modelId={modelId} model={model} className={className} />;
    }
  }

  // Wrap with ErrorBoundary for fault isolation
  const wrappedWidget = (
    <ErrorBoundary
      resetKeys={[model.state ? JSON.stringify(model.state) : modelId]}
      fallback={(error, reset) => (
        <WidgetErrorFallback
          error={error}
          modelId={modelId}
          modelName={model.modelName}
          onRetry={reset}
        />
      )}
      onError={(error, errorInfo) => {
        console.error(
          `[WidgetView] Error rendering widget ${modelId}:`,
          error,
          errorInfo.componentStack,
        );
      }}
    >
      {renderedWidget}
    </ErrorBoundary>
  );

  // Wrap with layout positioning if the widget has grid placement styles
  const hasChildStyles = Object.keys(childStyle).length > 0;
  if (hasChildStyles) {
    return <div style={childStyle}>{wrappedWidget}</div>;
  }

  return wrappedWidget;
}

export default WidgetView;
