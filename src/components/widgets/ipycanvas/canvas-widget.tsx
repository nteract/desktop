/**
 * ipycanvas Canvas widget.
 *
 * Renders an HTML <canvas> element and processes drawing commands sent from
 * Python via the ipycanvas binary protocol. Drawing commands arrive at each
 * canvas's comm_id, routed by createCanvasManagerRouter at the store level.
 *
 * @see https://ipycanvas.readthedocs.io/
 */

import { useCallback, useEffect, useRef } from "react";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";
import { getTypedArray, processCommands } from "./ipycanvas-commands";

// === CanvasWidget ===

export function CanvasWidget({ modelId, className }: WidgetComponentProps) {
  const { store, sendCustom } = useWidgetStoreRequired();

  const canvasRef = useRef<HTMLCanvasElement>(null);
  const ctxRef = useRef<CanvasRenderingContext2D | null>(null);
  // Track an async command processing chain so commands execute in order
  const processingRef = useRef<Promise<void>>(Promise.resolve());

  const width = useWidgetModelValue<number>(modelId, "width") ?? 200;
  const height = useWidgetModelValue<number>(modelId, "height") ?? 200;
  const sendClientReady = useWidgetModelValue<boolean>(modelId, "_send_client_ready_event") ?? true;
  const imageData = useWidgetModelValue<Uint8ClampedArray | null>(modelId, "image_data");

  // Initialize 2D context
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    ctxRef.current = canvas.getContext("2d");
  }, []);

  // Draw image_data when it changes (for restoring canvas state)
  useEffect(() => {
    if (!imageData || !ctxRef.current || !canvasRef.current) return;

    const blob = new Blob([imageData.buffer as ArrayBuffer]);
    const url = URL.createObjectURL(blob);
    const img = new Image();
    img.onload = () => {
      ctxRef.current?.drawImage(img, 0, 0);
      URL.revokeObjectURL(url);
    };
    img.src = url;
  }, [imageData]);

  // Subscribe to messages routed to this canvas's comm_id, then send
  // client_ready. Routing is handled by createCanvasManagerRouter at the
  // store level — this widget just processes what it receives.
  useEffect(() => {
    const unsubscribe = store.subscribeToCustomMessage(modelId, (content, buffers) => {
      const canvas = canvasRef.current;
      const ctx = ctxRef.current;
      if (!canvas || !ctx || !buffers || buffers.length === 0) return;

      // Chain command processing to maintain order
      processingRef.current = processingRef.current.then(async () => {
        try {
          // First buffer is the JSON-encoded commands
          const metadata = content as { dtype: string };
          const typedArray = getTypedArray(buffers[0], metadata);
          const jsonStr = new TextDecoder("utf-8").decode(typedArray);
          const commands = JSON.parse(jsonStr);

          // Remaining buffers are binary data for batch operations
          const dataBuffers = buffers.slice(1);

          await processCommands(ctx, commands, dataBuffers, canvas, modelId, true);
        } catch (err) {
          console.warn("[ipycanvas] Error processing commands:", err);
        }
      });
    });

    if (sendClientReady) {
      sendCustom(modelId, { event: "client_ready" });
    }

    return unsubscribe;
  }, [store, modelId, sendClientReady, sendCustom]);

  // Mouse event helpers
  const getCoordinates = useCallback((event: React.MouseEvent<HTMLCanvasElement>) => {
    const canvas = canvasRef.current;
    if (!canvas) return { x: 0, y: 0 };
    const rect = canvas.getBoundingClientRect();
    return {
      x: (canvas.width * (event.clientX - rect.left)) / rect.width,
      y: (canvas.height * (event.clientY - rect.top)) / rect.height,
    };
  }, []);

  const onMouseMove = useCallback(
    (event: React.MouseEvent<HTMLCanvasElement>) => {
      sendCustom(modelId, { event: "mouse_move", ...getCoordinates(event) });
    },
    [modelId, sendCustom, getCoordinates],
  );

  const onMouseDown = useCallback(
    (event: React.MouseEvent<HTMLCanvasElement>) => {
      canvasRef.current?.focus();
      sendCustom(modelId, { event: "mouse_down", ...getCoordinates(event) });
    },
    [modelId, sendCustom, getCoordinates],
  );

  const onMouseUp = useCallback(
    (event: React.MouseEvent<HTMLCanvasElement>) => {
      sendCustom(modelId, { event: "mouse_up", ...getCoordinates(event) });
    },
    [modelId, sendCustom, getCoordinates],
  );

  const onMouseOut = useCallback(
    (event: React.MouseEvent<HTMLCanvasElement>) => {
      sendCustom(modelId, { event: "mouse_out", ...getCoordinates(event) });
    },
    [modelId, sendCustom, getCoordinates],
  );

  const onWheel = useCallback(
    (event: React.WheelEvent<HTMLCanvasElement>) => {
      sendCustom(modelId, {
        event: "mouse_wheel",
        x: event.deltaX,
        y: event.deltaY,
      });
      event.preventDefault();
    },
    [modelId, sendCustom],
  );

  const onKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLCanvasElement>) => {
      event.preventDefault();
      event.stopPropagation();
      sendCustom(modelId, {
        event: "key_down",
        key: event.key,
        shift_key: event.shiftKey,
        ctrl_key: event.ctrlKey,
        meta_key: event.metaKey,
      });
    },
    [modelId, sendCustom],
  );

  return (
    <canvas
      ref={canvasRef}
      width={width}
      height={height}
      tabIndex={0}
      className={cn("block", className)}
      data-widget-id={modelId}
      data-widget-type="Canvas"
      onMouseMove={onMouseMove}
      onMouseDown={onMouseDown}
      onMouseUp={onMouseUp}
      onMouseOut={onMouseOut}
      onBlur={() => sendCustom(modelId, { event: "mouse_out", x: 0, y: 0 })}
      onWheel={onWheel}
      onKeyDown={onKeyDown}
    />
  );
}

// === CanvasManagerWidget ===

/**
 * Stub for CanvasManagerModel. Routing is handled at the store level
 * by createCanvasManagerRouter. CanvasManagerModel has _view_name: null
 * and is never rendered.
 */
export function CanvasManagerWidget(_props: WidgetComponentProps) {
  return null;
}
