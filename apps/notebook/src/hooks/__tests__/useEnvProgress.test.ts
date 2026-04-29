import { describe, expect, it } from "vite-plus/test";
import type { EnvProgressEvent } from "runtimed";
import { getStatusText, progressKey, projectEnvProgress } from "../useEnvProgress";

describe("getStatusText", () => {
  it("keeps error status concise for inline toolbar display", () => {
    const event: EnvProgressEvent = {
      env_type: "uv",
      phase: "error",
      message: "Failed to install dependencies: numpy build error",
    };

    expect(getStatusText(event)).toBe("Environment error");
  });

  it("projects daemon-authored offline hits as inactive progress", () => {
    const event: EnvProgressEvent = {
      env_type: "pixi",
      phase: "offline_hit",
    };

    expect(projectEnvProgress(event)).toMatchObject({
      isActive: false,
      envType: "pixi",
      phase: "offline_hit",
      statusText: "Using cached packages",
    });
  });

  it("projects download details from runtime state progress", () => {
    const event: EnvProgressEvent = {
      env_type: "conda",
      phase: "download_progress",
      completed: 3,
      total: 8,
      current_package: "numpy",
      bytes_downloaded: 1024,
      bytes_total: 4096,
      bytes_per_second: 512,
    };

    expect(projectEnvProgress(event)).toMatchObject({
      isActive: true,
      envType: "conda",
      phase: "download_progress",
      progress: { completed: 3, total: 8 },
      bytesPerSecond: 512,
      currentPackage: "numpy",
    });
  });

  it("uses a stable dismissal key across object field order", () => {
    const eventA = {
      env_type: "uv",
      phase: "download_progress",
      completed: 1,
      total: 3,
      current_package: "numpy",
      bytes_downloaded: 1024,
      bytes_total: 4096,
      bytes_per_second: 512,
    } satisfies EnvProgressEvent;

    const eventB = {
      phase: "download_progress",
      bytes_per_second: 512,
      bytes_total: 4096,
      bytes_downloaded: 1024,
      current_package: "numpy",
      total: 3,
      completed: 1,
      env_type: "uv",
    } satisfies EnvProgressEvent;

    expect(JSON.stringify(eventA)).not.toBe(JSON.stringify(eventB));
    expect(progressKey(eventA)).toBe(progressKey(eventB));
  });
});
