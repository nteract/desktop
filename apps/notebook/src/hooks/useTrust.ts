import { useNotebookHost } from "@nteract/notebook-host";
import type { TrustInfo, TyposquatWarning } from "@nteract/notebook-host";
import { useCallback, useEffect, useState } from "react";
import { logger } from "../lib/logger";
import { useRuntimeState } from "../lib/runtime-state";

export type { TrustInfo, TyposquatWarning };

/** Trust status from the backend */
export type TrustStatusType = TrustInfo["status"];

export function useTrust() {
  const host = useNotebookHost();
  const runtimeState = useRuntimeState();
  const runtimeTrustNeedsApproval = runtimeState.trust.needs_approval;

  const [trustInfo, setTrustInfo] = useState<TrustInfo | null>(null);
  const [typosquatWarnings, setTyposquatWarnings] = useState<TyposquatWarning[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Check trust status
  const checkTrust = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const info = await host.trust.verify();
      setTrustInfo(info);

      // Check for typosquats in all dependencies
      const allDeps = [...info.uv_dependencies, ...info.conda_dependencies];
      if (allDeps.length > 0) {
        const warnings = await host.deps.checkTyposquats(allDeps);
        setTyposquatWarnings(warnings);
      } else {
        setTyposquatWarnings([]);
      }

      return info;
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      setError(message);
      if (message === "Not connected to daemon") {
        logger.debug("Trust check deferred: daemon not yet connected");
      } else {
        logger.error("Failed to check trust:", e);
      }
      return null;
    } finally {
      setLoading(false);
    }
  }, [host]);

  // Approve the notebook (sign dependencies)
  const approveTrust = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      await host.trust.approve();
      // Re-check trust status after approval
      await checkTrust();
      return true;
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      setError(message);
      logger.error("Failed to approve trust:", e);
      return false;
    } finally {
      setLoading(false);
    }
  }, [host, checkTrust]);

  // Check trust on mount
  useEffect(() => {
    checkTrust();
  }, [checkTrust]);

  // Re-check trust when daemon (re)connects — handles the startup race where
  // the initial mount-time check fires before the relay handle is stored.
  useEffect(() => {
    return host.daemonEvents.onReady(() => {
      checkTrust();
    });
  }, [host, checkTrust]);

  // Computed properties
  const isTrusted = trustInfo?.status === "trusted" || trustInfo?.status === "no_dependencies";
  const needsApproval =
    trustInfo?.status === "untrusted" ||
    trustInfo?.status === "signature_invalid" ||
    runtimeTrustNeedsApproval; // From RuntimeStateDoc — arrives via sync, no race
  const hasDependencies = trustInfo?.status !== "no_dependencies";

  // Total dependency count
  const totalDependencies =
    (trustInfo?.uv_dependencies.length ?? 0) + (trustInfo?.conda_dependencies.length ?? 0);

  return {
    trustInfo,
    typosquatWarnings,
    loading,
    error,
    isTrusted,
    needsApproval,
    hasDependencies,
    totalDependencies,
    checkTrust,
    approveTrust,
  };
}
