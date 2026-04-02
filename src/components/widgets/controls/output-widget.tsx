/**
 * Output widget - renders captured Jupyter outputs.
 *
 * Maps to ipywidgets OutputModel (@jupyter-widgets/output).
 * Renders an array of Jupyter outputs using the OutputArea component.
 * Media rendering configuration (custom renderers, priority) is
 * inherited from MediaProvider context if present.
 *
 * Note: The Output widget protocol is particularly complex. Rather than
 * simply setting outputs on the model from Python, Jupyter sends outputs
 * as custom messages that must be accumulated client-side. This includes
 * handling clear_output(wait=True) which defers clearing until the next
 * output arrives. We sync rendered state back to the model to keep
 * Python's `out.outputs` in sync with what's displayed.
 */

import { useEffect, useRef, useState } from "react";
import type { JupyterOutput } from "@/components/cell/jupyter-output";
import {
  AnsiErrorOutput,
  AnsiStreamOutput,
} from "@/components/outputs/ansi-output";
import { MediaRouter } from "@/components/outputs/media-router";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import {
  useWidgetModelValue,
  useWidgetStoreRequired,
} from "../widget-store-context";

interface OutputCustomMessage {
  method?: unknown;
  output?: unknown;
  wait?: unknown;
}

function isJupyterOutput(value: unknown): value is JupyterOutput {
  if (!value || typeof value !== "object") return false;
  const output = value as Partial<JupyterOutput>;
  return (
    output.output_type === "execute_result" ||
    output.output_type === "display_data" ||
    output.output_type === "stream" ||
    output.output_type === "error"
  );
}

export function OutputWidget({ modelId, className }: WidgetComponentProps) {
  const { store, sendUpdate } = useWidgetStoreRequired();
  const stateOutputs =
    useWidgetModelValue<JupyterOutput[]>(modelId, "outputs") ?? [];
  const stateOutputsRef = useRef(stateOutputs);
  const shouldClearOnNextOutputRef = useRef(false);
  const [renderedOutputs, setRenderedOutputs] =
    useState<JupyterOutput[]>(stateOutputs);
  const renderedOutputsRef = useRef(renderedOutputs);

  useEffect(() => {
    stateOutputsRef.current = stateOutputs;
    renderedOutputsRef.current = stateOutputs;
    setRenderedOutputs(stateOutputs);
  }, [stateOutputs]);

  useEffect(() => {
    renderedOutputsRef.current = renderedOutputs;
  }, [renderedOutputs]);

  useEffect(() => {
    let replayingBufferedMessages = true;

    const unsubscribe = store.subscribeToCustomMessage(modelId, (content) => {
      const message = content as OutputCustomMessage;
      const method =
        typeof message.method === "string" ? message.method : undefined;

      // OutputModel may have already synchronized state.outputs.
      // Skip initial buffered custom messages in that case to avoid duplicates.
      if (replayingBufferedMessages && stateOutputsRef.current.length > 0) {
        return;
      }

      if (method === "clear_output") {
        const wait = Boolean(message.wait);
        if (wait) {
          shouldClearOnNextOutputRef.current = true;
        } else {
          shouldClearOnNextOutputRef.current = false;
          renderedOutputsRef.current = [];
          setRenderedOutputs([]);
          // Keep Python-side `out.outputs` in sync with displayed output state.
          sendUpdate(modelId, { outputs: [] });
        }
        return;
      }

      if (method !== "output" || !isJupyterOutput(message.output)) {
        return;
      }
      const nextOutput: JupyterOutput = message.output;

      const prev = renderedOutputsRef.current;
      const next = shouldClearOnNextOutputRef.current
        ? [nextOutput]
        : [...prev, nextOutput];
      shouldClearOnNextOutputRef.current = false;
      renderedOutputsRef.current = next;
      setRenderedOutputs(next);
      // Keep Python-side `out.outputs` in sync with displayed output state.
      sendUpdate(modelId, { outputs: next });
    });

    replayingBufferedMessages = false;
    return unsubscribe;
  }, [modelId, sendUpdate, store]);

  if (renderedOutputs.length === 0) {
    return null;
  }

  return (
    <div
      className={cn("widget-output", className)}
      data-widget-id={modelId}
      data-widget-type="Output"
    >
      {renderedOutputs.map((output, index) => {
        switch (output.output_type) {
          case "execute_result":
          case "display_data":
            return (
              <MediaRouter
                key={`output-${index}`}
                data={output.data}
                metadata={
                  output.metadata as Record<
                    string,
                    Record<string, unknown> | undefined
                  >
                }
              />
            );
          case "stream":
            return (
              <AnsiStreamOutput
                key={`output-${index}`}
                text={
                  Array.isArray(output.text)
                    ? output.text.join("")
                    : output.text
                }
                streamName={output.name}
              />
            );
          case "error":
            return (
              <AnsiErrorOutput
                key={`output-${index}`}
                ename={output.ename}
                evalue={output.evalue}
                traceback={output.traceback}
              />
            );
          default:
            return null;
        }
      })}
    </div>
  );
}

export default OutputWidget;
