import { open as openExternal } from "@tauri-apps/plugin-shell";
import { useCallback, useState } from "react";
import { TelemetryDisclosureCard } from "@/components/TelemetryDisclosureCard";
import { Switch } from "@/components/ui/switch";

interface PrivacySectionProps {
  telemetryEnabled: boolean;
  onTelemetryChange: (value: boolean) => void;
  installId: string;
  onRotate: () => Promise<string | null>;
  lastDaemonPingAt: number | null;
  lastAppPingAt: number | null;
  lastMcpPingAt: number | null;
}

const TELEMETRY_PAGE_URL = "https://nteract.io/telemetry";

function formatRelative(secs: number | null): string {
  if (secs === null) return "never";
  const now = Math.floor(Date.now() / 1000);
  const diff = now - secs;
  if (diff < 60) return `${diff}s ago`;
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  return `${Math.floor(diff / 86400)}d ago`;
}

function openOrFallThrough(url: string) {
  openExternal(url).catch(() => {
    // Tauri shell unavailable; the calling <a> tag's href covers the
    // non-Tauri case.
  });
}

export function PrivacySection({
  telemetryEnabled,
  onTelemetryChange,
  installId,
  onRotate,
  lastDaemonPingAt,
  lastAppPingAt,
  lastMcpPingAt,
}: PrivacySectionProps) {
  const [isRotating, setIsRotating] = useState(false);

  const handleRotate = useCallback(async () => {
    if (isRotating) return;
    const ok = window.confirm(
      "Rotate your install ID? This generates a new random identifier. " +
        "Your old rows on the server become unlinkable and age out at 400 days.",
    );
    if (!ok) return;
    setIsRotating(true);
    try {
      await onRotate();
    } finally {
      setIsRotating(false);
    }
  }, [isRotating, onRotate]);

  return (
    <div className="space-y-3">
      <span className="text-xs font-semibold text-muted-foreground uppercase tracking-wider">
        Privacy
      </span>

      <TelemetryDisclosureCard
        onOpenLearnMore={openOrFallThrough}
        footer={
          <div className="flex items-center justify-between pt-1">
            <span className="text-xs text-muted-foreground">
              Send anonymous daily ping
            </span>
            <Switch checked={telemetryEnabled} onCheckedChange={onTelemetryChange} />
          </div>
        }
      />

      <div className="space-y-1.5">
        <div className="flex items-center justify-between gap-3">
          <span className="text-xs text-muted-foreground shrink-0">Install ID</span>
          <code
            className="text-[11px] text-foreground truncate bg-muted/50 px-2 py-0.5 rounded"
            title={installId}
          >
            {installId || "(not yet set)"}
          </code>
          <button
            type="button"
            onClick={handleRotate}
            disabled={isRotating || !installId}
            className="text-xs text-primary underline hover:text-foreground disabled:opacity-50"
          >
            {isRotating ? "Rotating..." : "Rotate"}
          </button>
        </div>

        <div className="flex items-center justify-between">
          <span className="text-xs text-muted-foreground">Last ping</span>
          <span className="text-xs text-foreground tabular-nums">
            app {formatRelative(lastAppPingAt)} · daemon{" "}
            {formatRelative(lastDaemonPingAt)} · mcp {formatRelative(lastMcpPingAt)}
          </span>
        </div>
      </div>
    </div>
  );
}
