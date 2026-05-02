/**
 * @runtimed/node — ergonomic JS wrapper over the N-API binding.
 *
 * The native binding lives in `binding.cjs` + `runtimed-node.<triple>.node`.
 * This wrapper keeps the generated API intact and layers Node-friendly helpers
 * on top for agents and scripts.
 */
"use strict";

const binding = require("./binding.cjs");

const DEFAULT_PACKAGE_MANAGER = binding.PackageManager?.Uv ?? "uv";

function asArray(value) {
  if (value == null) return [];
  return Array.isArray(value) ? value : [value];
}

function normalizeCreateOptions(options) {
  if (options == null) return options;
  const normalized = { ...options };

  // Agent-friendly aliases. The native binding owns the canonical field names.
  if (normalized.dependencies == null && normalized.deps != null) {
    normalized.dependencies = normalized.deps;
  }
  if (normalized.dependencies == null && normalized.packages != null) {
    normalized.dependencies = normalized.packages;
  }
  delete normalized.deps;
  delete normalized.packages;

  // Let callers pass a human-readable session description without learning
  // Automerge peer-label terminology.
  if (normalized.description && !normalized.peerLabel) {
    normalized.peerLabel = `runtimed-node:${normalized.description}`;
  }
  delete normalized.description;

  return normalized;
}

function parseJsonField(value, fallback) {
  if (value == null || value === "") return fallback;
  if (typeof value !== "string") return value;
  try {
    return JSON.parse(value);
  } catch {
    return fallback;
  }
}

function decodeDataValue(value) {
  if (!value || typeof value !== "object" || !("type" in value)) return value;
  if (value.type === "binary" && typeof value.value === "string") {
    return Buffer.from(value.value, "base64");
  }
  return value.value;
}

function decodeMimeBundle(dataJson) {
  const raw = parseJsonField(dataJson, {});
  return Object.fromEntries(
    Object.entries(raw).map(([mime, value]) => [mime, decodeDataValue(value)]),
  );
}

function outputToObject(output) {
  return {
    ...output,
    data: decodeMimeBundle(output.dataJson),
    blobUrls: parseJsonField(output.blobUrlsJson, {}),
    blobPaths: parseJsonField(output.blobPathsJson, {}),
  };
}

function enrichResult(result) {
  const outputs = result.outputs.map(outputToObject);
  const text = outputs
    .map((output) => {
      if (typeof output.text === "string") return output.text;
      const plain = output.data?.["text/plain"];
      return typeof plain === "string" ? plain : null;
    })
    .filter(Boolean)
    .join("");

  const errors = outputs.filter((output) => output.outputType === "error");
  const richData = outputs
    .map((output) => output.data)
    .filter((data) => data && Object.keys(data).length > 0);

  return {
    ...result,
    outputs,
    text,
    errors,
    richData,
    ok: result.success && errors.length === 0,
  };
}

async function installDependencies(session, dependencies, options = {}) {
  const packages = asArray(dependencies).filter(Boolean);
  for (const pkg of packages) {
    await session.addUvDependency(String(pkg));
  }
  if (options.sync !== false && packages.length > 0) {
    await session.syncEnvironment();
  }
  return session;
}

function enhanceSession(session) {
  if (!session || session.__runtimedNodeEnhanced) return session;

  Object.defineProperties(session, {
    __runtimedNodeEnhanced: { value: true },
    install: {
      value(dependencies, options) {
        return installDependencies(session, dependencies, options);
      },
    },
    run: {
      async value(source, options = {}) {
        if (options.dependencies || options.deps || options.packages) {
          await installDependencies(
            session,
            options.dependencies ?? options.deps ?? options.packages,
            { sync: options.syncDependencies },
          );
        }
        const { enrich = true, ...runOptions } = options;
        delete runOptions.dependencies;
        delete runOptions.deps;
        delete runOptions.packages;
        delete runOptions.syncDependencies;
        const result = await session.runCell(source, runOptions);
        return enrich ? enrichResult(result) : result;
      },
    },
    wait: {
      async value(executionId, options = {}) {
        const { enrich = true, ...waitOptions } = options;
        const result = await session.waitForExecution(executionId, waitOptions);
        return enrich ? enrichResult(result) : result;
      },
    },
  });

  return session;
}

async function createNotebook(options) {
  const session = await binding.createNotebook(normalizeCreateOptions(options));
  return enhanceSession(session);
}

async function openNotebook(notebookId, options) {
  const session = await binding.openNotebook(notebookId, options);
  return enhanceSession(session);
}

async function runPython(source, options = {}) {
  const {
    close = true,
    create = {},
    dependencies,
    deps,
    packages,
    packageManager = DEFAULT_PACKAGE_MANAGER,
    ...runOptions
  } = options;
  const session = await createNotebook({
    runtime: "python",
    packageManager,
    dependencies: dependencies ?? deps ?? packages,
    ...create,
  });
  try {
    return await session.run(source, runOptions);
  } finally {
    if (close) await session.close();
  }
}

module.exports = {
  ...binding,
  PackageManager: binding.PackageManager ?? {
    Uv: "uv",
    Conda: "conda",
    Pixi: "pixi",
  },
  createNotebook,
  openNotebook,
  runPython,
  enhanceSession,
  enrichResult,
  outputToObject,
  decodeMimeBundle,
};
