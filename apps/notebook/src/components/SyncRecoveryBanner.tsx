/**
 * Transient banner surfaced when the WASM sync layer auto-recovers
 * from a failed `receive_sync_message` (doc rebuilt, sync state
 * normalized, recovery reply sent).
 *
 * The recovery itself is already handled inside the SyncEngine — this
 * component only exists so the event is visible to the user. Silent
 * recovery would otherwise be invisible, exactly the class of bug
 * that the widget-sync stall investigation was chasing down.
 *
 * Auto-dismisses after ~5 s so a lone blip doesn't linger. If another
 * recovery fires while the banner is up, the timer resets and the
 * count ticks up — a visible "this connection is unhealthy" signal.
 */

import { RefreshCw, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { SyncErrorEvent } from "runtimed";

/** How long the banner stays up after the latest recovery (ms). */
const DISMISS_DELAY_MS = 5_000;

interface SyncRecoveryBannerProps {
  /**
   * Latest sync-error event, or null when none has fired recently.
   * Each emission replaces this reference; the banner uses the
   * reference identity to reset its auto-dismiss timer so a burst
   * keeps it visible.
   */
  event: SyncErrorEvent | null;
  /** Manual dismiss handler (X button). */
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
    // visible. A fresh emission after the banner has dismissed
    // restarts the counter — otherwise a one-off recovery hours
    // earlier would make a new one-off recovery report itself as
    // the second in a burst.
    setCount((c) => (visible ? c + 1 : 1));

    const timer = window.setTimeout(() => {
      setVisible(false);
      // Reset the burst counter on dismissal so the next emission
      // starts fresh.
      setCount(0);
    }, DISMISS_DELAY_MS);

    return () => {
      window.clearTimeout(timer);
    };
    // `visible` intentionally omitted from deps: we only care about
    // its value at the instant the event changes.
    // biome-ignore lint/correctness/useExhaustiveDependencies: see above
  }, [event]);

  if (!visible || !event) return null;

  const label = docLabel(event.doc);
  const detail =
    count > 1
      ? `Recovered ${count} times recently — connection may be unhealthy.`
      : "Rebuilt from daemon snapshot.";

  return (
    <div className="flex items-center gap-2 bg-sky-600/90 px-3 py-1 text-xs text-white">
      <RefreshCw className="h-3 w-3 flex-shrink-0" />
      <span className="font-medium flex-shrink-0">Sync recovered</span>
      <span className="text-sky-200 flex-shrink-0">—</span>
      <span className="text-sky-100 truncate">
        {label} {detail}
      </span>
      <div className="ml-auto flex items-center gap-1 flex-shrink-0">
        {onDismiss && (
          <button
            type="button"
            onClick={() => {
              setVisible(false);
              onDismiss();
            }}
            className="rounded p-0.5 hover:bg-sky-500/50 transition-colors"
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
