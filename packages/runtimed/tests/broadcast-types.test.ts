/**
 * Tests for broadcast type guards.
 *
 * These guards narrow the untyped `broadcasts$` observable into typed
 * sub-streams. A bug here silently drops events (guard returns false
 * when it shouldn't) or lets the wrong type through (guard returns
 * true for a mismatched event). Both failure modes are quiet — no
 * runtime error, just missing kernel output or a TypeScript cast
 * that turns into NaN / undefined deep in a render.
 */

import { describe, expect, it } from "vite-plus/test";
import { isCommBroadcast } from "../src/broadcast-types";

// All guards share `hasBroadcastEvent` — exercise the invalid-payload
// matrix once so every guard inherits the coverage.
const INVALID_PAYLOADS: Array<[string, unknown]> = [
  ["null", null],
  ["undefined", undefined],
  ["number", 42],
  ["string", "comm"],
  ["boolean", true],
  ["array", ["comm"]],
  ["empty object", {}],
  ["object missing `event`", { msg_type: "comm_msg" }],
  ["event is not a string", { event: 7 }],
  ["event is an object", { event: { nested: "comm" } }],
];

describe("isCommBroadcast", () => {
  it("accepts a payload with the matching event", () => {
    // Only the `event` field is checked; the rest of the payload is
    // carried through as `any` into the narrowed type. The guard is
    // deliberately permissive so the daemon can add fields without
    // breaking the frontend — pin that behavior.
    expect(isCommBroadcast({ event: "comm" })).toBe(true);
    expect(isCommBroadcast({ event: "comm", extra: "ignored", nested: { a: 1 } })).toBe(true);
  });

  it.each(INVALID_PAYLOADS)("rejects %s", (_label, payload) => {
    expect(isCommBroadcast(payload)).toBe(false);
  });

  it("rejects payloads with a different event discriminator", () => {
    // Env progress used to ride the broadcast channel; it moved to
    // RuntimeStateDoc. If a future refactor accidentally starts sending
    // env_progress broadcasts again, make sure isCommBroadcast doesn't
    // absorb them.
    expect(isCommBroadcast({ event: "env_progress" })).toBe(false);
    expect(isCommBroadcast({ event: "unknown_future_event" })).toBe(false);
  });
});
