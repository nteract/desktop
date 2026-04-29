import { describe, expect, it } from "vite-plus/test";

import { DEFAULT_POOL_STATE, type PoolState, type RuntimePoolState } from "../src";

describe("PoolDoc state contract", () => {
  it("exports zeroed defaults for each runtime pool", () => {
    expect(DEFAULT_POOL_STATE).toEqual({
      uv: {
        available: 0,
        warming: 0,
        pool_size: 0,
        consecutive_failures: 0,
        retry_in_secs: 0,
      },
      conda: {
        available: 0,
        warming: 0,
        pool_size: 0,
        consecutive_failures: 0,
        retry_in_secs: 0,
      },
      pixi: {
        available: 0,
        warming: 0,
        pool_size: 0,
        consecutive_failures: 0,
        retry_in_secs: 0,
      },
    });
  });

  it("keeps default pool snapshots independently addressable", () => {
    expect(DEFAULT_POOL_STATE.uv).not.toBe(DEFAULT_POOL_STATE.conda);
    expect(DEFAULT_POOL_STATE.uv).not.toBe(DEFAULT_POOL_STATE.pixi);
    expect(DEFAULT_POOL_STATE.conda).not.toBe(DEFAULT_POOL_STATE.pixi);
  });

  it("accepts the daemon PoolDoc schema shape", () => {
    const uv: RuntimePoolState = {
      available: 2,
      warming: 1,
      pool_size: 3,
      consecutive_failures: 0,
      retry_in_secs: 0,
    };
    const conda: RuntimePoolState = {
      available: 0,
      warming: 0,
      pool_size: 0,
      error: "setup failed",
      error_kind: "setup_failed",
      failed_package: "pandas",
      consecutive_failures: 2,
      retry_in_secs: 30,
    };
    const pixi: RuntimePoolState = {
      available: 1,
      warming: 0,
      pool_size: 1,
      consecutive_failures: 0,
      retry_in_secs: 0,
    };
    const state: PoolState = { uv, conda, pixi };

    expect(state.uv.available).toBe(2);
    expect(state.conda.failed_package).toBe("pandas");
    expect(state.pixi.available).toBe(1);
  });
});
