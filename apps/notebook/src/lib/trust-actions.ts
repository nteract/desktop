import type { DependencyGuard, GuardedNotebookProvenance } from "runtimed";

export type PendingTrustAction =
  | {
      kind: "execute_cell";
      cellId: string;
      provenance: GuardedNotebookProvenance;
    }
  | {
      kind: "run_all";
      provenance: GuardedNotebookProvenance;
    }
  | { kind: "sync_deps"; provenance: DependencyGuard };
