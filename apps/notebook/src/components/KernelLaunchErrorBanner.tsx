import { AlertCircle, RotateCw, X } from "lucide-react";
import { KERNEL_ERROR_REASON, type RuntimeLifecycle } from "runtimed";
import { Button } from "@/components/ui/button";

/**
 * Decide whether the generic kernel-launch banner should render.
 *
 * Exported as a pure function so unit tests can exercise the gating
 * without mounting App.tsx (which needs a NotebookHost + WASM). The
 * call site in App.tsx composes this with `dismissedLaunchError` for
 * dismissal state.
 *
 * Rules:
 *
 * - Only show in `Error` state with non-empty `errorDetails`.
 * - Skip `MissingIpykernel`: toolbar renders a targeted "install
 *   ipykernel" prompt that already consumes the error message.
 * - Skip `runtime === "deno"`: toolbar renders the Deno
 *   "auto-install failed" prompt that already consumes it.
 *
 * Everything else — including `CondaEnvYmlMissing`, stderr tails from
 * generic subprocess crashes, env-build rate limits — falls through
 * to this banner. `CondaEnvYmlMissing`'s `error_details` carries the
 * conda env name + remediation command; surfacing that string in the
 * banner is a strict upgrade on the previous tooltip-only surface.
 */
export function shouldShowKernelLaunchErrorBanner(params: {
  lifecycle: RuntimeLifecycle;
  errorDetails: string | null;
  errorReason: string | null;
  runtime: string | null;
}): boolean {
  if (params.lifecycle.lifecycle !== "Error") return false;
  if (!params.errorDetails || params.errorDetails.length === 0) return false;
  if (params.errorReason === KERNEL_ERROR_REASON.MISSING_IPYKERNEL) return false;
  if (params.runtime === "deno") return false;
  return true;
}

interface KernelLaunchErrorBannerProps {
  /**
   * Stderr tail or other free-form details from the daemon's failed
   * launch. Usually multi-line. Rendered monospace + preserving
   * newlines so stack traces / subprocess errors stay readable.
   */
  errorDetails: string;
  onRetry: () => void;
  onDismiss: () => void;
}

/**
 * Banner surfaced when the daemon reports `RuntimeLifecycle::Error`
 * without a typed `KernelErrorReason`. Those typed cases
 * (`MissingIpykernel`, `CondaEnvYmlMissing`) have their own targeted
 * prompts; this one covers everything else — subprocess crashes,
 * import errors, rate-limited env builds, etc.
 *
 * App.tsx gates visibility on lifecycle + a non-typed reason and
 * resets the dismiss state when `errorDetails` changes, so a new
 * failure after a retry re-shows the banner.
 */
export function KernelLaunchErrorBanner({
  errorDetails,
  onRetry,
  onDismiss,
}: KernelLaunchErrorBannerProps) {
  return (
    <div className="flex items-start gap-3 border-b border-red-600/50 bg-red-600/10 px-3 py-2 text-xs text-red-900 dark:text-red-200">
      <AlertCircle className="h-4 w-4 flex-shrink-0 mt-0.5 text-red-600 dark:text-red-400" />
      <div className="min-w-0 flex-1">
        <div className="font-medium">Kernel failed to start</div>
        <pre className="mt-1 max-h-32 overflow-y-auto whitespace-pre-wrap break-words rounded bg-red-950/5 px-2 py-1 font-mono text-[11px] leading-snug text-red-950/90 dark:bg-red-950/30 dark:text-red-100/90">
          {errorDetails}
        </pre>
      </div>
      <div className="flex flex-shrink-0 items-center gap-1">
        <Button
          size="sm"
          variant="secondary"
          className="h-6 px-2 text-xs bg-red-100 text-red-900 hover:bg-red-200 dark:bg-red-900/40 dark:text-red-100 dark:hover:bg-red-900/60"
          onClick={onRetry}
        >
          <RotateCw className="h-3 w-3 mr-1" />
          Retry
        </Button>
        <button
          type="button"
          onClick={onDismiss}
          className="rounded p-0.5 text-red-700 transition-colors hover:bg-red-500/20 dark:text-red-300"
          aria-label="Dismiss"
        >
          <X className="h-3 w-3" />
        </button>
      </div>
    </div>
  );
}
