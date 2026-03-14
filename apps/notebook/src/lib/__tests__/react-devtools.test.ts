import { describe, expect, it } from "vitest";
import { resolveReactDevToolsConfig } from "../react-devtools";

describe("resolveReactDevToolsConfig", () => {
  it("uses the default standalone connection settings", () => {
    expect(resolveReactDevToolsConfig({})).toEqual({
      enabled: true,
      host: "localhost",
      port: 8097,
      useHttps: false,
      retryConnectionDelay: 5_000,
    });
  });

  it("honors explicit overrides", () => {
    expect(
      resolveReactDevToolsConfig({
        VITE_REACT_DEVTOOLS_HOST: "127.0.0.1",
        VITE_REACT_DEVTOOLS_PORT: "9000",
        VITE_REACT_DEVTOOLS_HTTPS: "true",
        VITE_REACT_DEVTOOLS_RETRY_DELAY_MS: "2500",
      }),
    ).toEqual({
      enabled: true,
      host: "127.0.0.1",
      port: 9000,
      useHttps: true,
      retryConnectionDelay: 2_500,
    });
  });

  it("falls back when numeric overrides are invalid", () => {
    expect(
      resolveReactDevToolsConfig({
        VITE_REACT_DEVTOOLS_PORT: "99999",
        VITE_REACT_DEVTOOLS_RETRY_DELAY_MS: "-1",
      }),
    ).toEqual({
      enabled: true,
      host: "localhost",
      port: 8097,
      useHttps: false,
      retryConnectionDelay: 5_000,
    });
  });

  it("allows disabling the auto-connect bootstrap", () => {
    expect(
      resolveReactDevToolsConfig({
        VITE_REACT_DEVTOOLS_DISABLED: "1",
      }).enabled,
    ).toBe(false);
  });
});
