import {
  DEFAULT_RUNTIME_STATE,
  deriveEnvManager,
  deriveRuntimeKind,
  type EnvManagerMetadataInputs,
  type ProjectContext,
  type RuntimeState,
} from "runtimed";
import { describe, expect, it } from "vite-plus/test";

const OBSERVED_AT = "2026-04-26T00:00:00Z";

const NO_METADATA: EnvManagerMetadataInputs = {
  isUvConfigured: false,
  isCondaConfigured: false,
  environmentYmlHasDeps: false,
  pixiHasDeps: false,
};

function stateWith(overrides: {
  env_source?: string;
  project_context?: ProjectContext;
}): RuntimeState {
  return {
    ...DEFAULT_RUNTIME_STATE,
    kernel: { ...DEFAULT_RUNTIME_STATE.kernel, env_source: overrides.env_source ?? "" },
    project_context: overrides.project_context ?? { state: "Pending" },
  };
}

function detectedCtx(kind: "PyprojectToml" | "PixiToml" | "EnvironmentYml"): ProjectContext {
  return {
    state: "Detected",
    project_file: {
      kind,
      absolute_path: `/proj/${kind}`,
      relative_to_notebook: kind,
    },
    parsed: {
      dependencies: [],
      dev_dependencies: [],
      requires_python: null,
      prerelease: null,
      extras: { kind: "None" },
    },
    observed_at: OBSERVED_AT,
  };
}

describe("deriveEnvManager", () => {
  it("prefers a running env_source over anything else", () => {
    // Even a conflicting project_context shouldn't flip the answer —
    // the daemon picked the env, so that's what's running.
    expect(
      deriveEnvManager(
        stateWith({ env_source: "pixi:toml", project_context: detectedCtx("PyprojectToml") }),
        { ...NO_METADATA, isUvConfigured: true },
      ),
    ).toBe("pixi");
    expect(deriveEnvManager(stateWith({ env_source: "conda:inline" }), NO_METADATA)).toBe("conda");
    expect(deriveEnvManager(stateWith({ env_source: "uv:pyproject" }), NO_METADATA)).toBe("uv");
  });

  it("falls back to inline metadata when env_source is empty", () => {
    const s = stateWith({});
    expect(deriveEnvManager(s, { ...NO_METADATA, isUvConfigured: true })).toBe("uv");
    expect(deriveEnvManager(s, { ...NO_METADATA, isCondaConfigured: true })).toBe("conda");
    expect(deriveEnvManager(s, { ...NO_METADATA, environmentYmlHasDeps: true })).toBe("conda");
    expect(deriveEnvManager(s, { ...NO_METADATA, pixiHasDeps: true })).toBe("pixi");
  });

  it("falls back to project_context.kind when no other signal", () => {
    // This is the whole point of the cutover: an untitled/bare notebook
    // opened inside a project dir should light up the right manager
    // even before the kernel has stamped metadata.
    expect(
      deriveEnvManager(stateWith({ project_context: detectedCtx("PyprojectToml") }), NO_METADATA),
    ).toBe("uv");
    expect(
      deriveEnvManager(stateWith({ project_context: detectedCtx("PixiToml") }), NO_METADATA),
    ).toBe("pixi");
    expect(
      deriveEnvManager(stateWith({ project_context: detectedCtx("EnvironmentYml") }), NO_METADATA),
    ).toBe("conda");
  });

  it("returns null when everything is empty", () => {
    expect(deriveEnvManager(stateWith({}), NO_METADATA)).toBeNull();
  });

  it("returns null for Pending / NotFound / Unreadable project_context with no other signal", () => {
    expect(
      deriveEnvManager(stateWith({ project_context: { state: "Pending" } }), NO_METADATA),
    ).toBeNull();
    expect(
      deriveEnvManager(
        stateWith({ project_context: { state: "NotFound", observed_at: OBSERVED_AT } }),
        NO_METADATA,
      ),
    ).toBeNull();
    expect(
      deriveEnvManager(
        stateWith({
          project_context: {
            state: "Unreadable",
            path: "/proj/pyproject.toml",
            reason: "bad",
            observed_at: OBSERVED_AT,
          },
        }),
        NO_METADATA,
      ),
    ).toBeNull();
  });

  it("inline metadata beats project_context fallback", () => {
    // The user has uv deps locally but the directory has a pixi.toml.
    // Inline metadata wins — the notebook is declaring what it wants.
    expect(
      deriveEnvManager(stateWith({ project_context: detectedCtx("PixiToml") }), {
        ...NO_METADATA,
        isUvConfigured: true,
      }),
    ).toBe("uv");
  });
});

describe("deriveRuntimeKind", () => {
  it("prefers detectedRuntime (WASM-resolved kernelspec)", () => {
    expect(deriveRuntimeKind(stateWith({}), "python", null)).toBe("python");
    expect(deriveRuntimeKind(stateWith({}), "deno", "python")).toBe("deno");
  });

  it("falls back to runtimeHint (daemon:ready payload)", () => {
    expect(deriveRuntimeKind(stateWith({}), null, "python")).toBe("python");
    expect(deriveRuntimeKind(stateWith({}), null, "deno")).toBe("deno");
  });

  it("falls back to project_context when no metadata yet", () => {
    // Any detected project file implies python — deno has no project file.
    expect(
      deriveRuntimeKind(stateWith({ project_context: detectedCtx("PyprojectToml") }), null, null),
    ).toBe("python");
    expect(
      deriveRuntimeKind(stateWith({ project_context: detectedCtx("PixiToml") }), null, null),
    ).toBe("python");
    expect(
      deriveRuntimeKind(stateWith({ project_context: detectedCtx("EnvironmentYml") }), null, null),
    ).toBe("python");
  });

  it("returns null when nothing identifies the runtime", () => {
    expect(deriveRuntimeKind(stateWith({}), null, null)).toBeNull();
    expect(
      deriveRuntimeKind(
        stateWith({ project_context: { state: "NotFound", observed_at: OBSERVED_AT } }),
        null,
        null,
      ),
    ).toBeNull();
  });

  it("ignores garbage strings from the hint channel", () => {
    // runtime_hint could theoretically be a malformed value; deriver
    // only accepts the two known runtimes.
    expect(deriveRuntimeKind(stateWith({}), "ruby", null)).toBeNull();
    expect(deriveRuntimeKind(stateWith({}), null, "foo")).toBeNull();
  });
});
