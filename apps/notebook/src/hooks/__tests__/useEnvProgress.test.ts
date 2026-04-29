import { describe, expect, it } from "vite-plus/test";
import type { EnvProgressEvent } from "runtimed";
import { getStatusText } from "../useEnvProgress";

describe("getStatusText", () => {
  it("keeps error status concise for inline toolbar display", () => {
    const event: EnvProgressEvent = {
      env_type: "uv",
      phase: "error",
      message: "Failed to install dependencies: numpy build error",
    };

    expect(getStatusText(event)).toBe("Environment error");
  });

  it("labels offline cache hits from the shared env progress phase union", () => {
    const event: EnvProgressEvent = {
      env_type: "pixi",
      phase: "offline_hit",
    };

    expect(getStatusText(event)).toBe("Using cached packages");
  });
});
