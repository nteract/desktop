import { AlertTriangleIcon, PackageIcon, ShieldAlertIcon } from "lucide-react";
import { useCallback } from "react";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import type { TrustInfo, TyposquatWarning } from "../hooks/useTrust";

interface TrustDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  trustInfo: TrustInfo | null;
  typosquatWarnings: TyposquatWarning[];
  onApprove: () => Promise<boolean>;
  onDecline: () => void;
  loading?: boolean;
  /** When true, shows daemon-specific messaging about auto-launch */
  daemonMode?: boolean;
}

/** Package list item with optional typosquat warning */
function PackageItem({ pkg, warning }: { pkg: string; warning?: TyposquatWarning }) {
  return (
    <div className="flex items-center gap-2 py-1.5 px-2">
      <PackageIcon className="size-4 shrink-0 text-muted-foreground" />
      <span className="font-mono text-sm truncate">{pkg}</span>
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
  onDecline,
  loading = false,
  daemonMode = false,
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

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg" showCloseButton={false} data-testid="trust-dialog">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <ShieldAlertIcon className="size-5 text-amber-500" />
            {isSignatureInvalid ? "Dependencies Modified" : "Review Dependencies"}
          </DialogTitle>
          <DialogDescription>
            {isSignatureInvalid
              ? "This notebook's dependencies have been modified since you last approved them. Review and approve to continue."
              : daemonMode
                ? "This notebook wants to install packages. Once approved, the kernel will start automatically."
                : "This notebook wants to install packages. Review them before running code."}
          </DialogDescription>
        </DialogHeader>

        <div className="max-h-[300px] overflow-y-auto space-y-4">
          {/* UV (PyPI) Dependencies */}
          {trustInfo && trustInfo.uv_dependencies.length > 0 && (
            <div>
              <h4 className="text-sm font-medium text-muted-foreground mb-2">PyPI Packages</h4>
              <div className="border rounded-md divide-y">
                {trustInfo.uv_dependencies.map((pkg) => (
                  <PackageItem key={pkg} pkg={pkg} warning={getWarning(pkg)} />
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
                  <PackageItem key={pkg} pkg={pkg} warning={getWarning(pkg)} />
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

        <DialogFooter className="gap-2 sm:gap-0">
          <Button
            variant="outline"
            onClick={handleDecline}
            disabled={loading}
            data-testid="trust-decline-button"
          >
            Don't Install
          </Button>
          <Button onClick={handleApprove} disabled={loading} data-testid="trust-approve-button">
            {loading ? "Approving..." : daemonMode ? "Trust & Start" : "Trust & Install"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
