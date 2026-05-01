import type { DependencyGuard, GuardedNotebookProvenance } from "runtimed";

export type PendingTrustAction =
  | {
      kind: "execute_cell";
      cellId: string;
      provenance: GuardedNotebookProvenance;
      dependencyFingerprint: string;
    }
  | {
      kind: "run_all";
      provenance: GuardedNotebookProvenance;
      dependencyFingerprint: string;
    }
  | { kind: "sync_deps"; provenance: DependencyGuard };

function assertNever(value: never): never {
  throw new Error(`Unhandled trust action kind: ${JSON.stringify(value)}`);
}

export function pendingActionDependencyFingerprint(
  action: PendingTrustAction | null,
): string | undefined {
  if (!action) return undefined;
  switch (action.kind) {
    case "execute_cell":
    case "run_all":
      return action.dependencyFingerprint;
    case "sync_deps":
      return action.provenance.dependency_fingerprint;
    default:
      return assertNever(action);
  }
}

export function refreshPendingActionDependencyFingerprint(
  action: PendingTrustAction,
  dependencyFingerprint: string,
): PendingTrustAction {
  switch (action.kind) {
    case "execute_cell":
    case "run_all":
      return {
        ...action,
        dependencyFingerprint,
      };
    case "sync_deps":
      return {
        ...action,
        provenance: {
          ...action.provenance,
          dependency_fingerprint: dependencyFingerprint,
        },
      };
    default:
      return assertNever(action);
  }
}
