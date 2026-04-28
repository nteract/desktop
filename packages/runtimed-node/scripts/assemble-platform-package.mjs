#!/usr/bin/env node

import { copyFileSync, existsSync, mkdirSync, readdirSync, rmSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const srcDir = join(packageRoot, "src");
const npmDir = join(packageRoot, "npm");

const binaries = readdirSync(srcDir).filter((file) => /^runtimed-node\..+\.node$/.test(file));

if (binaries.length !== 1) {
  console.error(
    `Expected exactly one built native binding in ${srcDir}, found ${binaries.length}: ${binaries.join(", ")}`,
  );
  console.error("Run pnpm --dir packages/runtimed-node build first.");
  process.exit(1);
}

const binary = binaries[0];
const target = binary.replace(/^runtimed-node\./, "").replace(/\.node$/, "");
const expectedTargetIndex = process.argv.indexOf("--expected-target");
const expectedTarget = expectedTargetIndex >= 0 ? process.argv[expectedTargetIndex + 1] : undefined;

if (expectedTargetIndex >= 0 && !expectedTarget) {
  console.error("--expected-target requires a target argument.");
  process.exit(1);
}

if (expectedTarget && expectedTarget !== target) {
  console.error(
    `Built native binding target ${target} did not match expected target ${expectedTarget}.`,
  );
  process.exit(1);
}

const platformPackageDir = join(npmDir, target);
const platformPackageJson = join(platformPackageDir, "package.json");

if (!existsSync(platformPackageJson)) {
  console.error(`No platform package manifest exists for ${target}: ${platformPackageJson}`);
  process.exit(1);
}

mkdirSync(platformPackageDir, { recursive: true });
for (const file of readdirSync(platformPackageDir)) {
  if (/^runtimed-node\..+\.node$/.test(file)) {
    rmSync(join(platformPackageDir, file));
  }
}

copyFileSync(join(srcDir, binary), join(platformPackageDir, binary));
console.log(`Copied ${binary} to ${platformPackageDir}`);

if (process.argv.includes("--pack-dry-run")) {
  const result = spawnSync("pnpm", ["--dir", platformPackageDir, "pack", "--dry-run"], {
    stdio: "inherit",
  });
  process.exit(result.status ?? 1);
}

console.log(`Prepared ${basename(platformPackageDir)} for packing.`);
