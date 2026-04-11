/**
 * Controller widget - gamepad input via Web Gamepad API.
 *
 * Maps to ipywidgets ControllerModel. Polls connected gamepads
 * and updates button/axis child widgets with current state.
 */

import { GamepadIcon } from "lucide-react";
import { useCallback, useEffect, useRef } from "react";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import type { WidgetComponentProps } from "../widget-registry";
import {
  parseModelRef,
  useWidgetModelValue,
  useWidgetStoreRequired,
} from "../widget-store-context";
import { WidgetView } from "../widget-view";

export function ControllerWidget({ modelId, className }: WidgetComponentProps) {
  // sendUpdate is now stable (useCommRouter uses refs internally)
  const { sendUpdate } = useWidgetStoreRequired();
  const animationRef = useRef<number | null>(null);
  const lastStateRef = useRef<{
    buttons: { pressed: boolean; value: number }[];
    axes: number[];
  } | null>(null);

  // Subscribe to widget state
  const index = useWidgetModelValue<number>(modelId, "index") ?? 0;
  const connected = useWidgetModelValue<boolean>(modelId, "connected") ?? false;
  const name = useWidgetModelValue<string>(modelId, "name") ?? "";
  const mapping = useWidgetModelValue<string>(modelId, "mapping") ?? "";
  const buttons = useWidgetModelValue<string[]>(modelId, "buttons") ?? [];
  const axes = useWidgetModelValue<string[]>(modelId, "axes") ?? [];

  // Ref for polling state values - these change during polling and shouldn't restart the rAF loop.
  // sendUpdate is stable so it doesn't need a ref.
  const stateRef = useRef({ connected, buttons, axes });

  // Keep state ref up-to-date without triggering the polling effect
  useEffect(() => {
    stateRef.current = { connected, buttons, axes };
  });

  // Poll gamepad state - only depends on index, modelId, sendUpdate to avoid loop restarts
  const pollGamepad = useCallback(() => {
    const gamepads = navigator.getGamepads();
    const gamepad = gamepads[index];
    const { connected, buttons, axes } = stateRef.current;

    if (gamepad) {
      // Check if we need to update connection state
      if (!connected) {
        sendUpdate(modelId, {
          connected: true,
          name: gamepad.id,
          mapping: gamepad.mapping,
          timestamp: gamepad.timestamp,
        });
      }

      const lastState = lastStateRef.current;
      let hasChanges = false;

      // Update button states
      gamepad.buttons.forEach((button, i) => {
        const buttonRef = buttons[i];
        if (!buttonRef) return;

        const buttonId = parseModelRef(buttonRef);
        if (!buttonId) return;

        const lastButton = lastState?.buttons[i];
        if (
          !lastButton ||
          lastButton.pressed !== button.pressed ||
          Math.abs(lastButton.value - button.value) > 0.01
        ) {
          sendUpdate(buttonId, {
            pressed: button.pressed,
            value: button.value,
          });
          hasChanges = true;
        }
      });

      // Update axis states
      gamepad.axes.forEach((axisValue, i) => {
        const axisRef = axes[i];
        if (!axisRef) return;

        const axisId = parseModelRef(axisRef);
        if (!axisId) return;

        const lastAxis = lastState?.axes[i];
        if (lastAxis === undefined || Math.abs(lastAxis - axisValue) > 0.01) {
          sendUpdate(axisId, { value: axisValue });
          hasChanges = true;
        }
      });

      // Store current state for comparison
      if (hasChanges || !lastState) {
        lastStateRef.current = {
          buttons: gamepad.buttons.map((b) => ({
            pressed: b.pressed,
            value: b.value,
          })),
          axes: [...gamepad.axes],
        };
      }

      // Update timestamp
      if (hasChanges) {
        sendUpdate(modelId, { timestamp: gamepad.timestamp });
      }
    } else if (connected) {
      // Gamepad disconnected
      sendUpdate(modelId, { connected: false });
      lastStateRef.current = null;
    }

    animationRef.current = requestAnimationFrame(pollGamepad);
  }, [index, modelId, sendUpdate]);

  // Start/stop polling based on component lifecycle
  useEffect(() => {
    // Start polling
    animationRef.current = requestAnimationFrame(pollGamepad);

    // Handle gamepad connect/disconnect events
    const handleConnect = (e: GamepadEvent) => {
      if (e.gamepad.index === index) {
        sendUpdate(modelId, {
          connected: true,
          name: e.gamepad.id,
          mapping: e.gamepad.mapping,
        });
      }
    };

    const handleDisconnect = (e: GamepadEvent) => {
      if (e.gamepad.index === index) {
        sendUpdate(modelId, { connected: false });
        lastStateRef.current = null;
      }
    };

    window.addEventListener("gamepadconnected", handleConnect);
    window.addEventListener("gamepaddisconnected", handleDisconnect);

    return () => {
      if (animationRef.current) {
        cancelAnimationFrame(animationRef.current);
      }
      window.removeEventListener("gamepadconnected", handleConnect);
      window.removeEventListener("gamepaddisconnected", handleDisconnect);
    };
  }, [index, modelId, pollGamepad, sendUpdate]);

  return (
    <div
      className={cn("flex flex-col gap-3 rounded-lg border p-4", className)}
      data-widget-id={modelId}
      data-widget-type="Controller"
    >
      {/* Header */}
      <div className="flex items-center gap-2">
        <GamepadIcon
          className={cn("size-5", connected ? "text-green-500" : "text-muted-foreground")}
        />
        <Label className="font-medium">{name || `Controller ${index}`}</Label>
        <span
          className={cn("ml-auto text-xs", connected ? "text-green-500" : "text-muted-foreground")}
        >
          {connected ? "Connected" : "Disconnected"}
        </span>
      </div>

      {connected && (
        <>
          {/* Buttons */}
          {buttons.length > 0 && (
            <div className="space-y-1">
              <Label className="text-xs text-muted-foreground">Buttons</Label>
              <div className="flex flex-wrap gap-1">
                {buttons.map((buttonRef, i) => {
                  const buttonId = parseModelRef(buttonRef);
                  return buttonId ? (
                    <WidgetView key={buttonId} modelId={buttonId} />
                  ) : (
                    <div
                      key={i}
                      className="flex size-8 items-center justify-center rounded-full border bg-muted text-xs"
                    >
                      {i}
                    </div>
                  );
                })}
              </div>
            </div>
          )}

          {/* Axes */}
          {axes.length > 0 && (
            <div className="space-y-1">
              <Label className="text-xs text-muted-foreground">Axes</Label>
              <div className="flex flex-col gap-1">
                {axes.map((axisRef, i) => {
                  const axisId = parseModelRef(axisRef);
                  return axisId ? (
                    <div key={axisId} className="flex items-center gap-2">
                      <span className="w-8 text-xs text-muted-foreground">{i}:</span>
                      <WidgetView modelId={axisId} />
                    </div>
                  ) : null;
                })}
              </div>
            </div>
          )}

          {/* Mapping info */}
          {mapping && <div className="text-xs text-muted-foreground">Mapping: {mapping}</div>}
        </>
      )}

      {!connected && (
        <div className="py-4 text-center text-sm text-muted-foreground">
          Press a button on a gamepad to connect
        </div>
      )}
    </div>
  );
}

export default ControllerWidget;
