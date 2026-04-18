/**
 * Transient banner surfaced when the WASM sync layer auto-recovers
 * from a failed sync message (doc rebuilt, sync state normalized,
 * recovery reply sent by the SyncEngine).
 *
 * The recovery itself is already complete by the time this fires —
 * the banner only exists so the event is visible to the user.
 * Silent recovery is exactly the class of bug the original widget
 * sync stall turned out to be.
 *
 * Auto-dismisses after ~5 s so a single blip doesn't linger. If
 * another recovery fires while the banner is still visible, the
 * timer resets and the counter ticks up — so a flapping connection
 * reads as "recovered N times recently" rather than implying the
 * first event repeats. The counter resets on auto-dismiss so a lone
 * recovery an hour later isn't mis-labeled as the 2nd in a burst.
 */

import { RefreshCw, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { SyncErrorEvent } from "runtimed";

/** How long the banner stays visible after the latest recovery (ms). */
const DISMISS_DELAY_MS = 5_000;

interface SyncRecoveryBannerProps {
  /**
   * Latest sync-error event, or null when none has fired recently.
   * The banner uses reference identity to detect new emissions — so
   * App-level wiring should `setEvent(e)` on each emission even if
   * the event's fields happen to match the previous one.
   */
  event: SyncErrorEvent | null;
  /** Optional manual dismiss handler (X button). */
  onDismiss?: () => void;
}

export function SyncRecoveryBanner({ event, onDismiss }: SyncRecoveryBannerProps) {
  const [visible, setVisible] = useState(false);
  const [count, setCount] = useState(0);
  const lastEventRef = useRef<SyncErrorEvent | null>(null);

  useEffect(() => {
    if (!event || event === lastEventRef.current) return;
    lastEventRef.current = event;
    setVisible(true);
    // Only accumulate the burst counter while the banner is still
    // up. A fresh emission after the banner dismissed starts a new
    // count of 1, so "recovered once an hour ago" + "recovered now"
    // doesn't read as a 2-event burst.
    setCount((c) => (visible ? c + 1 : 1));

    const timer = window.setTimeout(() => {
      setVisible(false);
      setCount(0);
    }, DISMISS_DELAY_MS);

    return () => window.clearTimeout(timer);
    // `visible` intentionally omitted from deps: only its value at
    // the instant the event reference changes matters here.
    // biome-ignore lint/correctness/useExhaustiveDependencies: see above
  }, [event]);

  if (!visible || !event) return null;

  const isStall = event.kind === "stall_detected";
  const headline = isStall ? "Sync stalled" : "Sync recovered";
  const detail = isStall
    ? "No response from daemon — forcing a resync."
    : count > 1
      ? `Recovered ${count} times recently — connection may be unhealthy.`
      : "Rebuilt from daemon snapshot.";
  const bg = isStall ? "bg-amber-600/90" : "bg-sky-600/90";
  const dot = isStall ? "text-amber-200" : "text-sky-200";
  const body = isStall ? "text-amber-100" : "text-sky-100";

  return (
    <div className={`flex items-center gap-2 ${bg} px-3 py-1 text-xs text-white`}>
      <RefreshCw className="h-3 w-3 flex-shrink-0" />
      <span className="font-medium flex-shrink-0">{headline}</span>
      <span className={`${dot} flex-shrink-0`}>—</span>
      <span className={`${body} truncate`}>
        {docLabel(event.doc)} {detail}
      </span>
      <div className="ml-auto flex items-center gap-1 flex-shrink-0">
        {onDismiss && (
          <button
            type="button"
            onClick={() => {
              setVisible(false);
              onDismiss();
            }}
            className={`rounded p-0.5 transition-colors ${
              isStall ? "hover:bg-amber-500/50" : "hover:bg-sky-500/50"
            }`}
            aria-label="Dismiss"
          >
            <X className="h-3 w-3" />
          </button>
        )}
      </div>
    </div>
  );
}

function docLabel(doc: SyncErrorEvent["doc"]): string {
  switch (doc) {
    case "notebook":
      return "Notebook document.";
    case "runtime_state":
      return "Runtime state.";
    case "pool_state":
      return "Pool state.";
  }
}
