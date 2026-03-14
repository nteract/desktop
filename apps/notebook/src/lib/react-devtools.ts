export interface ReactDevToolsEnv {
  readonly VITE_REACT_DEVTOOLS_DISABLED?: string;
  readonly VITE_REACT_DEVTOOLS_HOST?: string;
  readonly VITE_REACT_DEVTOOLS_PORT?: string;
  readonly VITE_REACT_DEVTOOLS_HTTPS?: string;
  readonly VITE_REACT_DEVTOOLS_RETRY_DELAY_MS?: string;
}

export interface ResolvedReactDevToolsConfig {
  enabled: boolean;
  host: string;
  port: number;
  useHttps: boolean;
  retryConnectionDelay: number;
}

const DEFAULT_HOST = "localhost";
const DEFAULT_PORT = 8097;
const DEFAULT_RETRY_DELAY_MS = 5_000;
const MIN_PORT = 1;
const MAX_PORT = 65_535;

function parseBoolean(value: string | undefined): boolean | undefined {
  if (!value) {
    return undefined;
  }

  switch (value.trim().toLowerCase()) {
    case "1":
    case "true":
    case "yes":
    case "on":
      return true;
    case "0":
    case "false":
    case "no":
    case "off":
      return false;
    default:
      return undefined;
  }
}

function parseIntegerInRange(
  value: string | undefined,
  min: number,
  max: number,
): number | undefined {
  if (!value) {
    return undefined;
  }

  const parsed = Number.parseInt(value, 10);
  if (!Number.isFinite(parsed) || parsed < min || parsed > max) {
    return undefined;
  }

  return parsed;
}

export function resolveReactDevToolsConfig(
  env: ReactDevToolsEnv,
): ResolvedReactDevToolsConfig {
  const host = env.VITE_REACT_DEVTOOLS_HOST?.trim() || DEFAULT_HOST;
  const port =
    parseIntegerInRange(env.VITE_REACT_DEVTOOLS_PORT, MIN_PORT, MAX_PORT) ??
    DEFAULT_PORT;
  const retryConnectionDelay =
    parseIntegerInRange(
      env.VITE_REACT_DEVTOOLS_RETRY_DELAY_MS,
      0,
      Number.MAX_SAFE_INTEGER,
    ) ?? DEFAULT_RETRY_DELAY_MS;

  return {
    enabled: parseBoolean(env.VITE_REACT_DEVTOOLS_DISABLED) !== true,
    host,
    port,
    useHttps: parseBoolean(env.VITE_REACT_DEVTOOLS_HTTPS) === true,
    retryConnectionDelay,
  };
}
