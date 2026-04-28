import { AlertTriangleIcon, CheckIcon, PackageIcon, ShieldAlertIcon } from "lucide-react";
import { useCallback } from "react";
import { Button } from "@/components/ui/button";
import type { TrustInfo, TyposquatWarning } from "../hooks/useTrust";
import { RuntimeDecisionDialog } from "./RuntimeDecisionDialog";

interface TrustDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  trustInfo: TrustInfo | null;
  typosquatWarnings: TyposquatWarning[];
  onApprove: () => Promise<boolean>;
  onApproveOnly?: () => Promise<boolean>;
  onDecline: () => void;
  loading?: boolean;
  /** When true, shows daemon-specific messaging about auto-launch */
  daemonMode?: boolean;
  approveLabel?: string;
  approveOnlyLabel?: string;
  description?: string;
  approvalError?: string | null;
}

/** Package list item with optional allowlist and typosquat status */
function PackageItem({
  pkg,
  warning,
  approved,
}: {
  pkg: string;
  warning?: TyposquatWarning;
  approved?: boolean;
}) {
  return (
    <div className="flex items-center gap-2 py-1.5 px-2">
      {approved ? (
        <CheckIcon className="size-4 shrink-0 text-emerald-600 dark:text-emerald-400" />
      ) : (
        <PackageIcon className="size-4 shrink-0 text-muted-foreground" />
      )}
      <span className="font-mono text-sm truncate">{pkg}</span>
      {approved && (
        <span className="text-xs text-emerald-700 dark:text-emerald-300 bg-emerald-50 dark:bg-emerald-900/30 px-1.5 py-0.5 rounded">
          approved
        </span>
      )}
      {warning && (
        <span className="inline-flex items-center gap-1 text-xs text-amber-600 dark:text-amber-400 bg-amber-50 dark:bg-amber-900/30 px-1.5 py-0.5 rounded">
          <AlertTriangleIcon className="size-3" />
          Similar to "{warning.similar_to}"
        </span>
      )}
    </div>
  );
}

export function TrustDialog({
  open,
  onOpenChange,
  trustInfo,
  typosquatWarnings,
  onApprove,
  onApproveOnly,
  onDecline,
  loading = false,
  daemonMode = false,
  approveLabel,
  approveOnlyLabel,
  description,
  approvalError,
}: TrustDialogProps) {
  const handleApprove = useCallback(async () => {
    const success = await onApprove();
    if (success) {
      onOpenChange(false);
    }
  }, [onApprove, onOpenChange]);

  const handleDecline = useCallback(() => {
    onDecline();
    onOpenChange(false);
  }, [onDecline, onOpenChange]);

  const handleApproveOnly = useCallback(async () => {
    const success = await (onApproveOnly ?? onApprove)();
    if (success) {
      onOpenChange(false);
    }
  }, [onApproveOnly, onApprove, onOpenChange]);

  // Build a map of package -> warning for quick lookup
  const warningMap = new Map<string, TyposquatWarning>();
  for (const warning of typosquatWarnings) {
    warningMap.set(warning.package.toLowerCase(), warning);
  }

  const getWarning = (pkg: string): TyposquatWarning | undefined => {
    const name = pkg
      .split(/[><=!~[;@]/)[0]
      .trim()
      .toLowerCase();
    return warningMap.get(name);
  };

  const hasTyposquats = typosquatWarnings.length > 0;
  const isSignatureInvalid = trustInfo?.status === "signature_invalid";
  const approvedUv = new Set(trustInfo?.approved_uv_dependencies ?? []);
  const approvedConda = new Set(trustInfo?.approved_conda_dependencies ?? []);
  const approvedPixi = new Set(trustInfo?.approved_pixi_dependencies ?? []);
  const approvedPixiPypi = new Set(trustInfo?.approved_pixi_pypi_dependencies ?? []);

  return (
    <RuntimeDecisionDialog
      open={open}
      onOpenChange={onOpenChange}
      testId="trust-dialog"
      icon={<ShieldAlertIcon className="size-5 text-amber-500" />}
      title={isSignatureInvalid ? "Dependencies Modified" : "Review Dependencies"}
      description={
        description ??
        (isSignatureInvalid
          ? "This notebook's dependencies have been modified since you last approved them. Review and approve to continue."
          : daemonMode
            ? "This notebook wants to install packages. Once approved, the kernel will start automatically."
            : "This notebook wants to install packages. Review them before running code.")
      }
      footer={
        <>
          <Button
            variant="outline"
            onClick={handleDecline}
            disabled={loading}
            data-testid="trust-decline-button"
          >
            Don't Install
          </Button>
          {onApproveOnly && (
            <Button
              variant="outline"
              onClick={handleApproveOnly}
              disabled={loading}
              data-testid="trust-approve-only-button"
            >
              {approveOnlyLabel ?? "Trust Notebook"}
            </Button>
          )}
          <Button onClick={handleApprove} disabled={loading} data-testid="trust-approve-button">
            {loading
              ? "Approving..."
              : (approveLabel ?? (daemonMode ? "Trust & Start" : "Trust & Install"))}
          </Button>
        </>
      }
    >
      <div className="max-h-[300px] overflow-y-auto space-y-4">
        {approvalError && (
          <div
            className="flex items-start gap-2 p-3 rounded-md bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800"
            role="alert"
          >
            <AlertTriangleIcon className="size-5 text-amber-600 dark:text-amber-400 shrink-0 mt-0.5" />
            <p className="text-sm text-amber-800 dark:text-amber-200">{approvalError}</p>
          </div>
        )}

        {/* UV (PyPI) Dependencies */}
        {trustInfo && trustInfo.uv_dependencies.length > 0 && (
          <div>
            <h4 className="text-sm font-medium text-muted-foreground mb-2">PyPI Packages</h4>
            <div className="border rounded-md divide-y">
              {trustInfo.uv_dependencies.map((pkg) => (
                <PackageItem
                  key={pkg}
                  pkg={pkg}
                  warning={getWarning(pkg)}
                  approved={approvedUv.has(pkg)}
                />
              ))}
            </div>
          </div>
        )}

        {/* Conda Dependencies */}
        {trustInfo && trustInfo.conda_dependencies.length > 0 && (
          <div>
            <h4 className="text-sm font-medium text-muted-foreground mb-2">
              Conda Packages
              {trustInfo.conda_channels.length > 0 && (
                <span className="font-normal text-xs ml-2">
                  ({trustInfo.conda_channels.join(", ")})
                </span>
              )}
            </h4>
            <div className="border rounded-md divide-y">
              {trustInfo.conda_dependencies.map((pkg) => (
                <PackageItem
                  key={pkg}
                  pkg={pkg}
                  warning={getWarning(pkg)}
                  approved={approvedConda.has(pkg)}
                />
              ))}
            </div>
          </div>
        )}

        {/* Pixi Dependencies */}
        {trustInfo && trustInfo.pixi_dependencies.length > 0 && (
          <div>
            <h4 className="text-sm font-medium text-muted-foreground mb-2">
              Pixi Packages
              {trustInfo.pixi_channels.length > 0 && (
                <span className="font-normal text-xs ml-2">
                  ({trustInfo.pixi_channels.join(", ")})
                </span>
              )}
            </h4>
            <div className="border rounded-md divide-y">
              {trustInfo.pixi_dependencies.map((pkg) => (
                <PackageItem
                  key={pkg}
                  pkg={pkg}
                  warning={getWarning(pkg)}
                  approved={approvedPixi.has(pkg)}
                />
              ))}
            </div>
          </div>
        )}

        {/* Pixi PyPI Dependencies */}
        {trustInfo && trustInfo.pixi_pypi_dependencies.length > 0 && (
          <div>
            <h4 className="text-sm font-medium text-muted-foreground mb-2">Pixi PyPI Packages</h4>
            <div className="border rounded-md divide-y">
              {trustInfo.pixi_pypi_dependencies.map((pkg) => (
                <PackageItem
                  key={pkg}
                  pkg={pkg}
                  warning={getWarning(pkg)}
                  approved={approvedPixiPypi.has(pkg)}
                />
              ))}
            </div>
          </div>
        )}

        {/* Typosquat Warning */}
        {hasTyposquats && (
          <div className="flex items-start gap-2 p-3 rounded-md bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800">
            <AlertTriangleIcon className="size-5 text-amber-600 dark:text-amber-400 shrink-0 mt-0.5" />
            <div className="text-sm">
              <p className="font-medium text-amber-800 dark:text-amber-200">
                Potential typosquatting detected
              </p>
              <p className="text-amber-700 dark:text-amber-300 mt-1">
                Some package names are similar to popular packages. Verify these are intentional
                before approving.
              </p>
            </div>
          </div>
        )}
      </div>
    </RuntimeDecisionDialog>
  );
}
