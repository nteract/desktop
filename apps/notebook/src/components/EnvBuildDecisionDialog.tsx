import { Copy, RotateCw, TerminalSquare } from "lucide-react";
import { useCallback, useMemo, useState } from "react";
import { Button } from "@/components/ui/button";
import { RuntimeDecisionDialog } from "./RuntimeDecisionDialog";

interface EnvBuildDecisionDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  errorDetails: string | null;
  onCreate: () => void;
  creating?: boolean;
}

export function extractCondaEnvCreateCommand(details: string | null): string | null {
  if (!details) return null;
  const match = details.match(/conda env create .+$/m);
  return match?.[0].trim() ?? null;
}

export function EnvBuildDecisionDialog({
  open,
  onOpenChange,
  errorDetails,
  onCreate,
  creating = false,
}: EnvBuildDecisionDialogProps) {
  const [copied, setCopied] = useState(false);
  const command = useMemo(() => extractCondaEnvCreateCommand(errorDetails), [errorDetails]);

  const copyCommand = useCallback(async () => {
    if (!command) return;
    await navigator.clipboard.writeText(command);
    setCopied(true);
  }, [command]);

  const create = useCallback(() => {
    setCopied(false);
    onCreate();
  }, [onCreate]);

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
          <Button onClick={create} disabled={creating} data-testid="env-build-create-button">
            <RotateCw className="mr-2 size-4" />
            {creating ? "Creating..." : "Create environment"}
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        <p className="text-sm text-muted-foreground">
          Create the declared environment to continue kernel launch. Future opens can launch
          automatically while the declared packages are approved.
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
