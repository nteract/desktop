/**
 * React context + hook for injecting the host platform.
 *
 * Wrap your root with `<NotebookHostProvider host={createTauriHost()}>` and
 * read the host anywhere in the tree with `useNotebookHost()`.
 *
 * Non-React callers (module-level helpers, sync stores) should accept a
 * host as a parameter rather than reaching for the context. That keeps
 * the dependency direction explicit.
 */

import { createContext, type ReactNode, useContext } from "react";
import type { NotebookHost } from "./types";

const NotebookHostContext = createContext<NotebookHost | null>(null);

export interface NotebookHostProviderProps {
  host: NotebookHost;
  children: ReactNode;
}

export function NotebookHostProvider({ host, children }: NotebookHostProviderProps) {
  return <NotebookHostContext.Provider value={host}>{children}</NotebookHostContext.Provider>;
}

export function useNotebookHost(): NotebookHost {
  const host = useContext(NotebookHostContext);
  if (!host) {
    throw new Error("useNotebookHost() must be called inside <NotebookHostProvider>");
  }
  return host;
}
