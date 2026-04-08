#!/usr/bin/env node
/**
 * nteract MCP Server Launcher
 *
 * Finds and launches the nteract MCP server (runt mcp) for the appropriate
 * channel (stable or nightly). The channel is determined by the NTERACT_CHANNEL
 * env var, set by the manifest's mcp_config.
 *
 * Acts as a line-based JSON-RPC proxy so it can transparently restart the
 * server when the daemon upgrades (exit code 75 / EX_TEMPFAIL). The proxy
 * captures the MCP initialize handshake and replays it to new child processes,
 * preserving client identity for notebook presence.
 *
 * Search order:
 * 1. PATH (covers /usr/local/bin/ where the app installer puts the binary)
 * 2. Platform-specific app bundle / install locations
 *
 * No fallback between channels — stable only looks for `runt`,
 * nightly only looks for `runt-nightly`.
 */

const { execFileSync, spawn } = require("node:child_process");
const { createInterface } = require("node:readline");
const { existsSync } = require("node:fs");
const { join } = require("node:path");

const channel = process.env.NTERACT_CHANNEL || "stable";
const binaryName = channel === "nightly" ? "runt-nightly" : "runt";
const appName = channel === "nightly" ? "nteract Nightly" : "nteract";

/** Exit code used by runt mcp when the daemon has been upgraded. */
const EX_TEMPFAIL = 75;

/** Circuit breaker: max restarts in the time window before giving up. */
const MAX_RESTARTS = 5;
const RESTART_WINDOW_MS = 30_000;

/** App bundle name candidates — must match runt_workspace::desktop_app_launch_candidates_for(). */
const appBundleNames =
  channel === "nightly"
    ? ["nteract Nightly", "nteract-nightly", "nteract (Nightly)"]
    : ["nteract"];

/** Platform-specific paths where the app installs the binary. */
function sidecarPaths() {
  const home = process.env.HOME || process.env.USERPROFILE || "";

  switch (process.platform) {
    case "darwin": {
      // The binary is in Contents/MacOS (not Resources/sidecars)
      // See: cli_install::get_bundled_runt_path
      const paths = [];
      for (const name of appBundleNames) {
        paths.push(`/Applications/${name}.app/Contents/MacOS/${binaryName}`);
        paths.push(
          join(home, `Applications/${name}.app/Contents/MacOS/${binaryName}`),
        );
      }
      return paths;
    }
    case "win32": {
      const localAppData =
        process.env.LOCALAPPDATA || join(home, "AppData/Local");
      const paths = [];
      for (const name of appBundleNames) {
        paths.push(join(localAppData, `${name}/${binaryName}.exe`));
        paths.push(join(localAppData, `Programs/${name}/${binaryName}.exe`));
      }
      return paths;
    }
    case "linux": {
      // See: find_bundled_runtimed() — searches /usr/share and /opt
      const paths = [join(home, `.local/bin/${binaryName}`)];
      for (const name of appBundleNames) {
        const slug = name.toLowerCase().replace(/ /g, "-");
        paths.push(`/usr/share/${slug}/${binaryName}`);
        paths.push(`/opt/${slug}/${binaryName}`);
      }
      return paths;
    }
    default:
      return [];
  }
}

/** Find the binary in PATH or platform-specific locations. */
function findBinary() {
  const whichCmd = process.platform === "win32" ? "where" : "which";
  const bin =
    process.platform === "win32" ? `${binaryName}.exe` : binaryName;

  // 1. Check PATH
  try {
    execFileSync(whichCmd, [bin], { stdio: "pipe" });
    return bin;
  } catch {
    // Not in PATH — check platform-specific locations
  }

  // 2. Check sidecar / install paths
  for (const candidate of sidecarPaths()) {
    if (existsSync(candidate)) return candidate;
  }

  return null;
}

// ── Proxy state ──────────────────────────────────────────────────────

/** Saved initialize request line (JSON string) from the MCP client. */
let savedInitRequest = null;
/** Saved initialized notification line (JSON string) from the MCP client. */
let savedInitNotification = null;
/** Whether we're waiting for the new child's initialize response after restart. */
let reinitializing = false;
/** The active child process. */
let child = null;
/** Circuit breaker: timestamps of recent restarts. */
const restartTimestamps = [];
/** Whether process.stdin has ended (client disconnected). */
let stdinEnded = false;

// ── Binary discovery ─────────────────────────────────────────────────

const binary = findBinary();

if (!binary) {
  process.stderr.write(
    `Error: ${binaryName} not found.\n\n` +
      `Install ${appName} from https://nteract.io to use this MCP server.\n` +
      `The app puts ${binaryName} on your PATH during installation.\n`,
  );
  process.exit(1);
}

// ── Child process management ─────────────────────────────────────────

function spawnChild() {
  const proc = spawn(binary, ["mcp"], {
    stdio: ["pipe", "pipe", "inherit"],
    env: process.env,
  });

  proc.on("error", (err) => {
    process.stderr.write(`Failed to start ${binary}: ${err.message}\n`);
    process.exit(1);
  });

  // Forward child stdout lines to the MCP client (process.stdout)
  const rl = createInterface({ input: proc.stdout, crlfDelay: Infinity });
  rl.on("line", (line) => {
    if (reinitializing) {
      // Discard the new child's initialize response — the client already
      // has capabilities from the original handshake.
      reinitializing = false;
      // Now send the saved initialized notification to complete the handshake.
      if (savedInitNotification && proc.stdin.writable) {
        proc.stdin.write(`${savedInitNotification}\n`);
      }
      return;
    }
    // Normal forwarding: child -> client
    process.stdout.write(`${line}\n`);
  });

  proc.on("exit", (code, signal) => {
    // Exit code 75 (EX_TEMPFAIL) means daemon upgraded — try to restart
    if (code === EX_TEMPFAIL && savedInitRequest && !stdinEnded) {
      if (canRestart()) {
        process.stderr.write("Daemon upgraded, restarting MCP server...\n");
        startChild();
        replayInitHandshake();
        return;
      }
      process.stderr.write(
        "Daemon upgraded but circuit breaker tripped — too many restarts.\n",
      );
    }

    // All other exits: forward to the MCP client as-is
    if (signal) {
      process.kill(process.pid, signal);
    } else {
      process.exit(code ?? 1);
    }
  });

  child = proc;
  return proc;
}

function startChild() {
  spawnChild();
}

/** Check circuit breaker: allow restart if < MAX_RESTARTS in the window. */
function canRestart() {
  const now = Date.now();
  // Prune old timestamps outside the window
  while (
    restartTimestamps.length > 0 &&
    now - restartTimestamps[0] > RESTART_WINDOW_MS
  ) {
    restartTimestamps.shift();
  }
  if (restartTimestamps.length >= MAX_RESTARTS) {
    return false;
  }
  restartTimestamps.push(now);
  return true;
}

/** Replay the saved initialize handshake to the new child. */
function replayInitHandshake() {
  if (!child?.stdin.writable || !savedInitRequest) return;
  reinitializing = true;
  child.stdin.write(`${savedInitRequest}\n`);
  // The child's initialize response will be intercepted and discarded
  // by the stdout line handler (reinitializing flag). After that,
  // the saved initialized notification is sent to complete the handshake.
}

// ── Client stdin -> child stdin forwarding ───────────────────────────

const stdinRl = createInterface({ input: process.stdin, crlfDelay: Infinity });

stdinRl.on("line", (line) => {
  // Capture the initialize handshake for replay on restart.
  // The initialize request contains clientInfo (name, title) used for
  // notebook presence — replaying it preserves the client's identity.
  if (!savedInitRequest) {
    try {
      const msg = JSON.parse(line);
      if (msg.method === "initialize") {
        savedInitRequest = line;
      }
    } catch {
      // Not valid JSON — forward anyway
    }
  } else if (!savedInitNotification) {
    try {
      const msg = JSON.parse(line);
      if (msg.method === "notifications/initialized") {
        savedInitNotification = line;
      }
    } catch {
      // Not valid JSON — forward anyway
    }
  }

  // Forward to child
  if (child?.stdin.writable) {
    child.stdin.write(`${line}\n`);
  }
});

stdinRl.on("close", () => {
  stdinEnded = true;
  child?.kill();
});

// ── Start ────────────────────────────────────────────────────────────

startChild();
