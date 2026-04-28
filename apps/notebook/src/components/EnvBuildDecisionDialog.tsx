import { Copy, RotateCw, TerminalSquare } from "lucide-react";
import { useCallback, useMemo, useState } from "react";
import { Button } from "@/components/ui/button";
import { RuntimeDecisionDialog } from "./RuntimeDecisionDialog";

interface EnvBuildDecisionDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  errorDetails: string | null;
  onRetry: () => void;
}

export function extractCondaEnvCreateCommand(details: string | null): string | null {
  if (!details) return null;
  const match = details.match(/conda env create -f .+$/m);
  return match?.[0].trim() ?? null;
}

export function EnvBuildDecisionDialog({
  open,
  onOpenChange,
  errorDetails,
  onRetry,
}: EnvBuildDecisionDialogProps) {
  const [copied, setCopied] = useState(false);
  const command = useMemo(() => extractCondaEnvCreateCommand(errorDetails), [errorDetails]);

  const copyCommand = useCallback(async () => {
    if (!command) return;
    await navigator.clipboard.writeText(command);
    setCopied(true);
  }, [command]);

  const retry = useCallback(() => {
    setCopied(false);
    onRetry();
  }, [onRetry]);

  return (
    <RuntimeDecisionDialog
      open={open}
      onOpenChange={onOpenChange}
      testId="env-build-decision-dialog"
      icon={<TerminalSquare className="size-5 text-amber-500" />}
      title="Build environment.yml environment"
      description="This notebook declares a conda environment that is not available on this machine."
      footer={
        <>
          <Button
            variant="outline"
            onClick={() => onOpenChange(false)}
            data-testid="env-build-cancel-button"
          >
            Cancel
          </Button>
          <Button
            variant="outline"
            onClick={copyCommand}
            disabled={!command}
            data-testid="env-build-copy-button"
          >
            <Copy className="mr-2 size-4" />
            {copied ? "Copied" : "Copy command"}
          </Button>
          <Button onClick={retry} data-testid="env-build-retry-button">
            <RotateCw className="mr-2 size-4" />
            Retry
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        <p className="text-sm text-muted-foreground">
          Build the declared environment in a terminal, then retry kernel launch.
        </p>
        {errorDetails && (
          <pre className="max-h-40 overflow-y-auto whitespace-pre-wrap break-words rounded-md border bg-muted/50 p-3 font-mono text-xs leading-relaxed">
            {errorDetails}
          </pre>
        )}
      </div>
    </RuntimeDecisionDialog>
  );
}
