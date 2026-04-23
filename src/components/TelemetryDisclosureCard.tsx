import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

interface TelemetryDisclosureCardProps {
  className?: string;
  footer?: ReactNode;
  /**
   * Optional URL opener so consumers can wire the Learn more link to the
   * host's `externalLinks.open` implementation. The default handler just
   * follows the `href` (unit tests, web contexts).
   */
  onOpenLearnMore?: (url: string) => void;
}

const LEARN_MORE_URL = "https://nteract.io/telemetry";

/**
 * One-source-of-truth disclosure card for telemetry.
 *
 * Rendered in:
 *  - Onboarding (step 2, above the two consent buttons).
 *  - Settings → Privacy (alongside the revisit toggle).
 *
 * The Learn more link points at https://nteract.io/telemetry — the
 * canonical public page that explains what's sent, what's never sent,
 * retention, and user rights. This card only carries the minimum
 * disclosure; clicking through is how the user gets the full picture.
 *
 * Consumers supply `onOpenLearnMore` to route the click through the
 * host's URL opener (e.g. `openUrl` from apps/notebook/src/lib/open-url).
 * The default behavior falls back to the `href` so unit tests and any
 * non-Tauri host still work.
 */
export function TelemetryDisclosureCard({
  className,
  footer,
  onOpenLearnMore,
}: TelemetryDisclosureCardProps) {
  const handleLearnMore = (e: React.MouseEvent<HTMLAnchorElement>) => {
    if (!onOpenLearnMore) return;
    e.preventDefault();
    onOpenLearnMore(LEARN_MORE_URL);
  };

  return (
    <div className={cn("rounded-lg border p-4 bg-muted/40 space-y-2", className)}>
      <div className="text-[10px] uppercase tracking-[0.14em] text-primary/80 font-semibold">
        One anonymous daily ping
      </div>
      <p className="text-sm text-foreground leading-6">
        Version, platform, architecture. No names, no paths, nothing about your notebooks.
      </p>
      <a
        href={LEARN_MORE_URL}
        onClick={handleLearnMore}
        rel="noreferrer"
        target="_blank"
        className="inline-block text-xs text-primary underline hover:text-foreground"
      >
        Read the full details
      </a>
      {footer ? <div className="pt-1">{footer}</div> : null}
    </div>
  );
}
