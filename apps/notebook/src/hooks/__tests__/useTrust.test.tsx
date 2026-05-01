import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vite-plus/test";
import { useTrust } from "../useTrust";

const mocks = vi.hoisted(() => ({
  approve: vi.fn(),
  checkTyposquats: vi.fn().mockResolvedValue([]),
  host: undefined as
    | {
        trust: { approve: ReturnType<typeof vi.fn> };
        deps: { checkTyposquats: ReturnType<typeof vi.fn> };
      }
    | undefined,
  uvDependencies: {
    dependencies: ["requests"],
  },
  condaDependencies: {
    dependencies: [],
    channels: [],
  },
  pixiDeps: {
    dependencies: [],
    pypiDependencies: [],
    channels: [],
  },
}));

mocks.host = {
  trust: {
    approve: mocks.approve,
  },
  deps: {
    checkTyposquats: mocks.checkTyposquats,
  },
};

vi.mock("@nteract/notebook-host", () => ({
  useNotebookHost: () => mocks.host,
}));

vi.mock("../../lib/runtime-state", () => ({
  useRuntimeState: () => ({
    trust: {
      status: "untrusted",
      needs_approval: true,
      approved_uv_dependencies: [],
      approved_conda_dependencies: [],
      approved_pixi_dependencies: [],
      approved_pixi_pypi_dependencies: [],
    },
  }),
  useRuntimeStateLoaded: () => true,
}));

vi.mock("../useDependencies", () => ({
  useDependencies: () => ({
    dependencies: mocks.uvDependencies,
  }),
}));

vi.mock("../useCondaDependencies", () => ({
  useCondaDependencies: () => ({
    dependencies: mocks.condaDependencies,
  }),
}));

vi.mock("../../lib/notebook-metadata", () => ({
  usePixiDeps: () => mocks.pixiDeps,
}));

describe("useTrust", () => {
  it("surfaces approval failures and clears stale approval errors before retry", async () => {
    mocks.approve
      .mockRejectedValueOnce(
        new Error("Dependencies changed while the trust dialog was open. Review before approving."),
      )
      .mockResolvedValueOnce(undefined);

    const { result } = renderHook(() => useTrust());

    await act(async () => {
      await expect(result.current.approveTrust({ observedHeads: ["head-v1"] })).resolves.toBe(
        false,
      );
    });

    expect(result.current.approvalError).toBe(
      "Dependencies changed while the trust dialog was open. Review before approving.",
    );

    await act(async () => {
      await expect(result.current.approveTrust({ observedHeads: ["head-v2"] })).resolves.toBe(true);
    });
    expect(result.current.approvalError).toBeNull();
  });
});
