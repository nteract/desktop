/**
 * Output widget - renders captured Jupyter outputs.
 *
 * Maps to ipywidgets OutputModel (@jupyter-widgets/output).
 * Renders an array of Jupyter outputs using the OutputArea component.
 * Media rendering configuration (custom renderers, priority) is
 * inherited from MediaProvider context if present.
 *
 * The daemon handles all output capture and kernel synchronization:
 * - Captured outputs are stored as manifest hashes in the CRDT
 * - The daemon resolves hashes and sends state to kernel directly
 * - The frontend reads outputs from WidgetStore (fed by CRDT watcher)
 * No frontend echo or custom message accumulation is needed.
 */

import type { JupyterOutput } from "@/components/cell/jupyter-output";
import { AnsiErrorOutput, AnsiStreamOutput } from "@/components/outputs/ansi-output";
import { MediaRouter } from "@/components/outputs/media-router";
import { ErrorBoundary } from "@/lib/error-boundary";
import { OutputErrorFallback } from "@/lib/output-error-fallback";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue } from "../widget-store-context";

/**
 * Render a single Jupyter output by type.
 * Mirrors the in-DOM path from OutputArea but without isolation
 * (the Output widget already runs inside an iframe).
 */
function renderWidgetOutput(output: JupyterOutput) {
  switch (output.output_type) {
    case "execute_result":
    case "display_data":
      return (
        <MediaRouter
          data={output.data}
          metadata={output.metadata as Record<string, Record<string, unknown> | undefined>}
        />
      );
    case "stream":
      return (
        <AnsiStreamOutput
          text={Array.isArray(output.text) ? output.text.join("") : output.text}
          streamName={output.name}
        />
      );
    case "error":
      return (
        <AnsiErrorOutput ename={output.ename} evalue={output.evalue} traceback={output.traceback} />
      );
    default:
      return null;
  }
}

export function OutputWidget({ modelId, className }: WidgetComponentProps) {
  const outputs = useWidgetModelValue<JupyterOutput[]>(modelId, "outputs") ?? [];

  if (outputs.length === 0) {
    return null;
  }

  return (
    <div
      className={cn("widget-output", className)}
      data-widget-id={modelId}
      data-widget-type="Output"
    >
      {outputs.map((output, index) => (
        <ErrorBoundary
          key={`output-${index}`}
          resetKeys={[JSON.stringify(output)]}
          fallback={(error, reset) => (
            <OutputErrorFallback error={error} outputIndex={index} onRetry={reset} />
          )}
          onError={(error, errorInfo) => {
            console.error(
              `[OutputWidget] Error rendering output ${index}:`,
              error,
              errorInfo.componentStack,
            );
          }}
        >
          {renderWidgetOutput(output)}
        </ErrorBoundary>
      ))}
    </div>
  );
}

export default OutputWidget;
