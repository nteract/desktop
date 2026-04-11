import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useCallback, useEffect, useState } from "react";
import { logger } from "../lib/logger";
import { useRuntimeState } from "../lib/runtime-state";

/** Trust status from the backend */
export type TrustStatusType = "trusted" | "untrusted" | "signature_invalid" | "no_dependencies";

export interface TrustInfo {
  status: TrustStatusType;
  uv_dependencies: string[];
  conda_dependencies: string[];
  conda_channels: string[];
}

export interface TyposquatWarning {
  package: string;
  similar_to: string;
  distance: number;
}

export function useTrust() {
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
      const info = await invoke<TrustInfo>("verify_notebook_trust");
      setTrustInfo(info);

      // Check for typosquats in all dependencies
      const allDeps = [...info.uv_dependencies, ...info.conda_dependencies];
      if (allDeps.length > 0) {
        const warnings = await invoke<TyposquatWarning[]>("check_typosquats", {
          packages: allDeps,
        });
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
  }, []);

  // Approve the notebook (sign dependencies)
  const approveTrust = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      await invoke("approve_notebook_trust");
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
  }, [checkTrust]);

  // Check trust on mount
  useEffect(() => {
    checkTrust();
  }, [checkTrust]);

  // Re-check trust when daemon (re)connects — handles the startup race where
  // the initial mount-time check fires before the relay handle is stored.
  useEffect(() => {
    const webview = getCurrentWebview();
    const unlistenReady = webview.listen("daemon:ready", () => {
      checkTrust();
    });
    return () => {
      unlistenReady.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [checkTrust]);

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
