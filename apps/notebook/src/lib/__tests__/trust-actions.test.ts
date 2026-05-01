import { describe, expect, it } from "vite-plus/test";
import {
  pendingActionDependencyFingerprint,
  refreshPendingActionDependencyFingerprint,
  type PendingTrustAction,
} from "../trust-actions";

const observedHeads = ["head-a"];

describe("trust-actions", () => {
  it("refreshes execute-cell action dependency fingerprints", () => {
    const action: PendingTrustAction = {
      kind: "execute_cell",
      cellId: "cell-1",
      provenance: { observed_heads: observedHeads },
      dependencyFingerprint: "deps-old",
    };

    const refreshed = refreshPendingActionDependencyFingerprint(action, "deps-current");

    expect(pendingActionDependencyFingerprint(refreshed)).toBe("deps-current");
    expect(refreshed).toMatchObject({
      kind: "execute_cell",
      cellId: "cell-1",
      provenance: { observed_heads: observedHeads },
    });
  });

  it("refreshes run-all action dependency fingerprints", () => {
    const action: PendingTrustAction = {
      kind: "run_all",
      provenance: { observed_heads: observedHeads },
      dependencyFingerprint: "deps-old",
    };

    const refreshed = refreshPendingActionDependencyFingerprint(action, "deps-current");

    expect(pendingActionDependencyFingerprint(refreshed)).toBe("deps-current");
    expect(refreshed).toMatchObject({
      kind: "run_all",
      provenance: { observed_heads: observedHeads },
    });
  });

  it("refreshes sync action dependency fingerprints inside the dependency guard", () => {
    const action: PendingTrustAction = {
      kind: "sync_deps",
      provenance: {
        observed_heads: observedHeads,
        dependency_fingerprint: "deps-old",
      },
    };

    const refreshed = refreshPendingActionDependencyFingerprint(action, "deps-current");

    expect(pendingActionDependencyFingerprint(refreshed)).toBe("deps-current");
    expect(refreshed).toMatchObject({
      kind: "sync_deps",
      provenance: {
        observed_heads: observedHeads,
        dependency_fingerprint: "deps-current",
      },
    });
  });
});
