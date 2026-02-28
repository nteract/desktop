/**
 * Hook for managing notebook dependencies via daemon.
 *
 * Dependencies are stored in the daemon's Automerge document and sync
 * automatically across all windows. This replaces direct NotebookState
 * manipulation for dependency operations.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useCallback, useEffect, useState } from "react";
import type {
  CondaDependencies,
  DaemonBroadcast,
  UvDependencies,
} from "../types";

/** Result from get_deps_via_daemon command */
interface DaemonDepsResult {
  uv: UvDependencies | null;
  conda: CondaDependencies | null;
}

export function useDaemonDependencies() {
  const [uvDeps, setUvDeps] = useState<UvDependencies | null>(null);
  const [condaDeps, setCondaDeps] = useState<CondaDependencies | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Subscribe to deps_changed broadcasts from daemon
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;

    const setup = async () => {
      unlisten = await listen<DaemonBroadcast>("daemon:broadcast", (event) => {
        if (event.payload.event === "deps_changed") {
          setUvDeps(event.payload.uv);
          setCondaDeps(event.payload.conda);
          setError(null);
        }
      });
    };

    setup();
    return () => {
      unlisten?.();
    };
  }, []);

  // Initial fetch on mount
  useEffect(() => {
    const fetchDeps = async () => {
      try {
        const result = await invoke<DaemonDepsResult>("get_deps_via_daemon");
        setUvDeps(result.uv);
        setCondaDeps(result.conda);
      } catch (e) {
        console.error("[daemon-deps] Failed to fetch initial deps:", e);
        // Don't set error - initial fetch failure is expected if not connected
      }
    };

    fetchDeps();
  }, []);

  // Re-fetch deps (called after reconnect or manual refresh)
  const refreshDeps = useCallback(async () => {
    try {
      const result = await invoke<DaemonDepsResult>("get_deps_via_daemon");
      setUvDeps(result.uv);
      setCondaDeps(result.conda);
      setError(null);
    } catch (e) {
      console.error("[daemon-deps] Failed to refresh deps:", e);
      setError(String(e));
    }
  }, []);

  // ── UV Dependencies ────────────────────────────────────────────

  const setUvDependencies = useCallback(
    async (dependencies: string[], requiresPython?: string) => {
      setLoading(true);
      setError(null);
      try {
        await invoke("set_uv_deps_via_daemon", {
          dependencies,
          requiresPython: requiresPython ?? null,
        });
        // State will be updated by broadcast
      } catch (e) {
        console.error("[daemon-deps] Failed to set UV deps:", e);
        setError(String(e));
      } finally {
        setLoading(false);
      }
    },
    [],
  );

  const addUvDependency = useCallback(async (pkg: string) => {
    if (!pkg.trim()) return;
    setLoading(true);
    setError(null);
    try {
      await invoke("add_uv_dep_via_daemon", { package: pkg.trim() });
      // State will be updated by broadcast
    } catch (e) {
      console.error("[daemon-deps] Failed to add UV dep:", e);
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  const removeUvDependency = useCallback(async (pkg: string) => {
    setLoading(true);
    setError(null);
    try {
      await invoke("remove_uv_dep_via_daemon", { package: pkg });
      // State will be updated by broadcast
    } catch (e) {
      console.error("[daemon-deps] Failed to remove UV dep:", e);
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  // ── Conda Dependencies ─────────────────────────────────────────

  const setCondaDependencies = useCallback(
    async (dependencies: string[], channels: string[], python?: string) => {
      setLoading(true);
      setError(null);
      try {
        await invoke("set_conda_deps_via_daemon", {
          dependencies,
          channels,
          python: python ?? null,
        });
        // State will be updated by broadcast
      } catch (e) {
        console.error("[daemon-deps] Failed to set Conda deps:", e);
        setError(String(e));
      } finally {
        setLoading(false);
      }
    },
    [],
  );

  const addCondaDependency = useCallback(async (pkg: string) => {
    if (!pkg.trim()) return;
    setLoading(true);
    setError(null);
    try {
      await invoke("add_conda_dep_via_daemon", { package: pkg.trim() });
      // State will be updated by broadcast
    } catch (e) {
      console.error("[daemon-deps] Failed to add Conda dep:", e);
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  const removeCondaDependency = useCallback(async (pkg: string) => {
    setLoading(true);
    setError(null);
    try {
      await invoke("remove_conda_dep_via_daemon", { package: pkg });
      // State will be updated by broadcast
    } catch (e) {
      console.error("[daemon-deps] Failed to remove Conda dep:", e);
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  // ── Computed State ─────────────────────────────────────────────

  const hasUvDeps = uvDeps !== null && uvDeps.dependencies.length > 0;
  const hasCondaDeps = condaDeps !== null && condaDeps.dependencies.length > 0;

  return {
    // UV dependencies
    uvDeps,
    hasUvDeps,
    setUvDependencies,
    addUvDependency,
    removeUvDependency,

    // Conda dependencies
    condaDeps,
    hasCondaDeps,
    setCondaDependencies,
    addCondaDependency,
    removeCondaDependency,

    // State
    loading,
    error,
    refreshDeps,
  };
}
