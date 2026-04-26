import { useNotebookHost } from "@nteract/notebook-host";
import type { TrustInfo, TyposquatWarning } from "@nteract/notebook-host";
import { useCallback, useEffect, useMemo, useState } from "react";
import { logger } from "../lib/logger";
import { useRuntimeState, useRuntimeStateLoaded } from "../lib/runtime-state";
import { usePixiDeps } from "../lib/notebook-metadata";
import { useCondaDependencies } from "./useCondaDependencies";
import { useDependencies } from "./useDependencies";

export type { TrustInfo, TyposquatWarning };

/** Trust status from the backend */
export type TrustStatusType = TrustInfo["status"];

interface ApproveTrustOptions {
  dependencyFingerprint?: string;
}

export function useTrust() {
  const host = useNotebookHost();
  const runtimeState = useRuntimeState();
  // Until the first RuntimeStateDoc frame lands, `runtimeState.trust` is
  // the static `DEFAULT_RUNTIME_STATE` value (`status: "no_dependencies"`).
  // Treating that as authoritative would fail-open the kernel-launch
  // trust gate — `tryStartKernel` reads `no_dependencies` as trusted and
  // would fire `LaunchKernel`, which the daemon honors by marking trust
  // approved before resolving. Hold `trustInfo` at `null` until the
  // daemon has actually spoken; call sites already fail-closed on null.
  const runtimeLoaded = useRuntimeStateLoaded();
  const { dependencies: uvDeps } = useDependencies();
  const { dependencies: condaDeps } = useCondaDependencies();
  const pixiDeps = usePixiDeps();

  const [typosquatWarnings, setTyposquatWarnings] = useState<TyposquatWarning[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Compose TrustInfo from RuntimeStateDoc + dep hooks. Daemon is the sole
  // writer of `trust.status`; deps are synced via the notebook CRDT. No
  // Tauri round-trip.
  const trustInfo: TrustInfo | null = useMemo(() => {
    if (!runtimeLoaded) return null;
    const uvList = uvDeps?.dependencies ?? [];
    const condaList = condaDeps?.dependencies ?? [];
    const channels = condaDeps?.channels ?? [];
    const pixiList = pixiDeps?.dependencies ?? [];
    const pixiPypiList = pixiDeps?.pypiDependencies ?? [];
    const pixiChannels = pixiDeps?.channels ?? [];
    return {
      status: runtimeState.trust.status,
      uv_dependencies: uvList,
      conda_dependencies: condaList,
      conda_channels: channels,
      pixi_dependencies: pixiList,
      pixi_pypi_dependencies: pixiPypiList,
      pixi_channels: pixiChannels,
    };
  }, [
    runtimeLoaded,
    runtimeState.trust.status,
    uvDeps?.dependencies,
    condaDeps?.dependencies,
    condaDeps?.channels,
    pixiDeps?.dependencies,
    pixiDeps?.pypiDependencies,
    pixiDeps?.channels,
  ]);

  // Typosquat check is the only piece still on the host. Reruns whenever
  // the effective dep list changes.
  const uvList = trustInfo?.uv_dependencies;
  const condaList = trustInfo?.conda_dependencies;
  const pixiList = trustInfo?.pixi_dependencies;
  const pixiPypiList = trustInfo?.pixi_pypi_dependencies;
  useEffect(() => {
    if (!uvList || !condaList || !pixiList || !pixiPypiList) return;
    const allDeps = [...uvList, ...condaList, ...pixiList, ...pixiPypiList];
    if (allDeps.length === 0) {
      setTyposquatWarnings([]);
      return;
    }
    let cancelled = false;
    setLoading(true);
    host.deps
      .checkTyposquats(allDeps)
      .then((warnings) => {
        if (!cancelled) setTyposquatWarnings(warnings);
      })
      .catch((e) => {
        if (!cancelled) {
          const message = e instanceof Error ? e.message : String(e);
          setError(message);
          logger.error("Failed to check typosquats:", e);
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [host, uvList, condaList, pixiList, pixiPypiList]);

  // Approve the notebook (sign dependencies)
  const approveTrust = useCallback(
    async (options?: ApproveTrustOptions) => {
      setLoading(true);
      setError(null);
      try {
        await host.trust.approve(options);
        return true;
      } catch (e) {
        const message = e instanceof Error ? e.message : String(e);
        setError(message);
        logger.error("Failed to approve trust:", e);
        return false;
      } finally {
        setLoading(false);
      }
    },
    [host],
  );

  // Computed properties. While `trustInfo` is null (daemon hasn't pushed
  // a state yet), nothing is known — default everything to the safe side:
  // untrusted, no deps, no approval pending. `needsApproval` stays false
  // to avoid flashing the trust dialog on a stale default; `tryStartKernel`
  // gates on `checkTrust()` returning non-null and will show the dialog
  // once real state lands.
  const isTrusted = trustInfo?.status === "trusted" || trustInfo?.status === "no_dependencies";
  const needsApproval = trustInfo
    ? trustInfo.status === "untrusted" ||
      trustInfo.status === "signature_invalid" ||
      runtimeState.trust.needs_approval
    : false;
  const hasDependencies = trustInfo ? trustInfo.status !== "no_dependencies" : false;
  const totalDependencies = trustInfo
    ? trustInfo.uv_dependencies.length +
      trustInfo.conda_dependencies.length +
      trustInfo.pixi_dependencies.length +
      trustInfo.pixi_pypi_dependencies.length
    : 0;

  return {
    trustInfo,
    typosquatWarnings,
    loading,
    error,
    isTrusted,
    needsApproval,
    hasDependencies,
    totalDependencies,
    // Returns the current composed TrustInfo, or null if the daemon has
    // not yet pushed a RuntimeStateDoc snapshot. Callers that gate kernel
    // launch on trust must fail-closed on null.
    checkTrust: useCallback(() => Promise.resolve(trustInfo), [trustInfo]),
    approveTrust,
  };
}
