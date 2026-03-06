import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import { logger } from "../lib/logger";

interface GitInfo {
  branch: string;
  commit: string;
  description: string | null;
}

interface DaemonInfo {
  version: string;
  socket_path: string;
  is_dev_mode: boolean;
}

export function useGitInfo() {
  const [gitInfo, setGitInfo] = useState<GitInfo | null>(null);

  useEffect(() => {
    invoke<GitInfo | null>("get_git_info")
      .then(setGitInfo)
      .catch((e) => {
        logger.error("Failed to get git info:", e);
      });
  }, []);

  return gitInfo;
}

export function useDaemonInfo() {
  const [daemonInfo, setDaemonInfo] = useState<DaemonInfo | null>(null);

  useEffect(() => {
    invoke<DaemonInfo | null>("get_daemon_info")
      .then(setDaemonInfo)
      .catch((e) => {
        logger.error("Failed to get daemon info:", e);
      });
  }, []);

  return daemonInfo;
}
