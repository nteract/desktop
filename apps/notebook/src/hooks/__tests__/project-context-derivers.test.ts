import type { ProjectContext } from "runtimed";
import { describe, expect, it } from "vite-plus/test";
import { deriveEnvironmentYml } from "../useCondaDependencies";
import { derivePyproject } from "../useDependencies";
import { derivePixiInfo } from "../usePixiDetection";

const OBSERVED_AT = "2026-04-25T00:00:00Z";

function pyprojectCtx(
  overrides: {
    dependencies?: string[];
    dev_dependencies?: string[];
    requires_python?: string | null;
    absolute_path?: string;
    relative_path?: string;
  } = {},
): ProjectContext {
  return {
    state: "Detected",
    project_file: {
      kind: "PyprojectToml",
      absolute_path: overrides.absolute_path ?? "/proj/pyproject.toml",
      relative_to_notebook: overrides.relative_path ?? "pyproject.toml",
    },
    parsed: {
      dependencies: overrides.dependencies ?? [],
      dev_dependencies: overrides.dev_dependencies ?? [],
      requires_python: overrides.requires_python ?? null,
      prerelease: null,
      extras: { kind: "None" },
    },
    observed_at: OBSERVED_AT,
  };
}

function pixiCtx(
  overrides: {
    dependencies?: string[];
    pypi_dependencies?: string[];
    channels?: string[];
    requires_python?: string | null;
  } = {},
): ProjectContext {
  return {
    state: "Detected",
    project_file: {
      kind: "PixiToml",
      absolute_path: "/proj/pixi.toml",
      relative_to_notebook: "pixi.toml",
    },
    parsed: {
      dependencies: overrides.dependencies ?? [],
      dev_dependencies: [],
      requires_python: overrides.requires_python ?? null,
      prerelease: null,
      extras: {
        kind: "Pixi",
        channels: overrides.channels ?? [],
        pypi_dependencies: overrides.pypi_dependencies ?? [],
      },
    },
    observed_at: OBSERVED_AT,
  };
}

function envYmlCtx(
  overrides: {
    dependencies?: string[];
    pip?: string[];
    channels?: string[];
    requires_python?: string | null;
  } = {},
): ProjectContext {
  return {
    state: "Detected",
    project_file: {
      kind: "EnvironmentYml",
      absolute_path: "/proj/environment.yml",
      relative_to_notebook: "environment.yml",
    },
    parsed: {
      dependencies: overrides.dependencies ?? [],
      dev_dependencies: [],
      requires_python: overrides.requires_python ?? null,
      prerelease: null,
      extras: {
        kind: "EnvironmentYml",
        channels: overrides.channels ?? [],
        pip: overrides.pip ?? [],
      },
    },
    observed_at: OBSERVED_AT,
  };
}

describe("derivePyproject", () => {
  it("returns nulls for Pending", () => {
    expect(derivePyproject({ state: "Pending" })).toEqual({
      pyprojectInfo: null,
      pyprojectDeps: null,
    });
  });

  it("returns nulls for NotFound", () => {
    expect(derivePyproject({ state: "NotFound", observed_at: OBSERVED_AT })).toEqual({
      pyprojectInfo: null,
      pyprojectDeps: null,
    });
  });

  it("returns nulls for Unreadable", () => {
    expect(
      derivePyproject({
        state: "Unreadable",
        path: "/proj/pyproject.toml",
        reason: "bad toml",
        observed_at: OBSERVED_AT,
      }),
    ).toEqual({ pyprojectInfo: null, pyprojectDeps: null });
  });

  it("returns nulls when the detected project is pixi", () => {
    expect(derivePyproject(pixiCtx())).toEqual({
      pyprojectInfo: null,
      pyprojectDeps: null,
    });
  });

  it("derives info + deps from a detected pyproject", () => {
    const { pyprojectInfo, pyprojectDeps } = derivePyproject(
      pyprojectCtx({
        dependencies: ["pandas>=2.0", "numpy"],
        dev_dependencies: ["pytest"],
        requires_python: ">=3.11",
        absolute_path: "/work/demo/pyproject.toml",
        relative_path: "pyproject.toml",
      }),
    );
    expect(pyprojectInfo).toEqual({
      path: "/work/demo/pyproject.toml",
      relative_path: "pyproject.toml",
      project_name: null,
      has_dependencies: true,
      dependency_count: 2,
      has_dev_dependencies: true,
      requires_python: ">=3.11",
      has_venv: false,
    });
    expect(pyprojectDeps).toEqual({
      path: "/work/demo/pyproject.toml",
      relative_path: "pyproject.toml",
      project_name: null,
      dependencies: ["pandas>=2.0", "numpy"],
      dev_dependencies: ["pytest"],
      requires_python: ">=3.11",
      index_url: null,
    });
  });

  it("reports has_dependencies=false and has_dev_dependencies=false when empty", () => {
    const { pyprojectInfo } = derivePyproject(pyprojectCtx());
    expect(pyprojectInfo?.has_dependencies).toBe(false);
    expect(pyprojectInfo?.dependency_count).toBe(0);
    expect(pyprojectInfo?.has_dev_dependencies).toBe(false);
  });
});

describe("derivePixiInfo", () => {
  it("returns null for Pending / NotFound / Unreadable", () => {
    expect(derivePixiInfo({ state: "Pending" })).toBeNull();
    expect(derivePixiInfo({ state: "NotFound", observed_at: OBSERVED_AT })).toBeNull();
    expect(
      derivePixiInfo({
        state: "Unreadable",
        path: "/proj/pixi.toml",
        reason: "bad",
        observed_at: OBSERVED_AT,
      }),
    ).toBeNull();
  });

  it("returns null when the detected project is pyproject, not pixi", () => {
    expect(derivePixiInfo(pyprojectCtx())).toBeNull();
  });

  it("derives PixiInfo with channels and pypi counts", () => {
    const info = derivePixiInfo(
      pixiCtx({
        dependencies: ["python", "numpy"],
        pypi_dependencies: ["requests", "rich"],
        channels: ["conda-forge", "bioconda"],
        requires_python: "3.11.*",
      }),
    );
    expect(info).toEqual({
      path: "/proj/pixi.toml",
      relative_path: "pixi.toml",
      workspace_name: null,
      has_dependencies: true,
      dependency_count: 2,
      has_pypi_dependencies: true,
      pypi_dependency_count: 2,
      python: "3.11.*",
      channels: ["conda-forge", "bioconda"],
    });
  });

  it("reports empty counts but does not return null for an empty pixi project", () => {
    const info = derivePixiInfo(pixiCtx());
    expect(info).not.toBeNull();
    expect(info?.has_dependencies).toBe(false);
    expect(info?.has_pypi_dependencies).toBe(false);
    expect(info?.channels).toEqual([]);
  });

  it("surfaces empty channels/pypi when a pixi ctx somehow carries non-Pixi extras", () => {
    // Defensive: the parser guarantees Pixi→Pixi extras, but the union
    // technically allows other shapes. Empty lists beat a crash.
    const info = derivePixiInfo({
      state: "Detected",
      project_file: {
        kind: "PixiToml",
        absolute_path: "/proj/pixi.toml",
        relative_to_notebook: "pixi.toml",
      },
      parsed: {
        dependencies: ["numpy"],
        dev_dependencies: [],
        requires_python: null,
        prerelease: null,
        extras: { kind: "None" },
      },
      observed_at: OBSERVED_AT,
    });
    expect(info?.channels).toEqual([]);
    expect(info?.has_pypi_dependencies).toBe(false);
  });
});

describe("deriveEnvironmentYml", () => {
  it("returns nulls for Pending / NotFound / Unreadable", () => {
    expect(deriveEnvironmentYml({ state: "Pending" })).toEqual({
      environmentYmlInfo: null,
      environmentYmlDeps: null,
    });
    expect(deriveEnvironmentYml({ state: "NotFound", observed_at: OBSERVED_AT })).toEqual({
      environmentYmlInfo: null,
      environmentYmlDeps: null,
    });
    expect(
      deriveEnvironmentYml({
        state: "Unreadable",
        path: "/proj/environment.yml",
        reason: "bad",
        observed_at: OBSERVED_AT,
      }),
    ).toEqual({ environmentYmlInfo: null, environmentYmlDeps: null });
  });

  it("returns nulls when the detected project is pixi", () => {
    expect(deriveEnvironmentYml(pixiCtx())).toEqual({
      environmentYmlInfo: null,
      environmentYmlDeps: null,
    });
  });

  it("derives info + deps with channels and pip sublist", () => {
    const { environmentYmlInfo, environmentYmlDeps } = deriveEnvironmentYml(
      envYmlCtx({
        dependencies: ["python=3.11", "numpy"],
        pip: ["requests", "flask"],
        channels: ["conda-forge"],
        requires_python: "3.11",
      }),
    );
    expect(environmentYmlInfo).toEqual({
      path: "/proj/environment.yml",
      relative_path: "environment.yml",
      name: null,
      has_dependencies: true,
      dependency_count: 2,
      has_pip_dependencies: true,
      pip_dependency_count: 2,
      python: "3.11",
      channels: ["conda-forge"],
    });
    expect(environmentYmlDeps).toEqual({
      path: "/proj/environment.yml",
      relative_path: "environment.yml",
      name: null,
      dependencies: ["python=3.11", "numpy"],
      pip_dependencies: ["requests", "flask"],
      python: "3.11",
      channels: ["conda-forge"],
    });
  });

  it("reports empty sublists without crashing", () => {
    const { environmentYmlInfo, environmentYmlDeps } = deriveEnvironmentYml(envYmlCtx());
    expect(environmentYmlInfo?.has_dependencies).toBe(false);
    expect(environmentYmlInfo?.has_pip_dependencies).toBe(false);
    expect(environmentYmlDeps?.dependencies).toEqual([]);
    expect(environmentYmlDeps?.pip_dependencies).toEqual([]);
    expect(environmentYmlDeps?.channels).toEqual([]);
  });
});
