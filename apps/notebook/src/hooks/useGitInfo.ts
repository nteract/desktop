import { useNotebookHost } from "@nteract/notebook-host";
import { useEffect, useState } from "react";
import type { DaemonInfo, GitInfo } from "@nteract/notebook-host";
import { logger } from "../lib/logger";

export function useGitInfo() {
  const host = useNotebookHost();
  const [gitInfo, setGitInfo] = useState<GitInfo | null>(null);

  useEffect(() => {
    host.system
      .getGitInfo()
      .then(setGitInfo)
      .catch((e) => {
        logger.error("Failed to get git info:", e);
      });
  }, [host]);

  return gitInfo;
}

export function useDaemonInfo() {
  const host = useNotebookHost();
  const [daemonInfo, setDaemonInfo] = useState<DaemonInfo | null>(null);

  useEffect(() => {
    host.daemon
      .getInfo()
      .then(setDaemonInfo)
      .catch((e) => {
        logger.error("Failed to get daemon info:", e);
      });
  }, [host]);

  return daemonInfo;
}
