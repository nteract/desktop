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
import {
  isCommBroadcast,
  isEnvProgressBroadcast,
  isNotebookAutosavedBroadcast,
  isPathChangedBroadcast,
} from "../src/broadcast-types";

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

const GUARDS = [
  ["isCommBroadcast", isCommBroadcast, "comm"],
  ["isEnvProgressBroadcast", isEnvProgressBroadcast, "env_progress"],
  ["isPathChangedBroadcast", isPathChangedBroadcast, "path_changed"],
  ["isNotebookAutosavedBroadcast", isNotebookAutosavedBroadcast, "notebook_autosaved"],
] as const;

describe("broadcast type guards", () => {
  describe.each(GUARDS)("%s", (_name, guard, event) => {
    it("accepts a payload with the matching event", () => {
      // Only the `event` field is checked; the rest of the payload is
      // carried through as `any` into the narrowed type. The guard is
      // deliberately permissive so the daemon can add fields without
      // breaking the frontend — pin that behavior.
      expect(guard({ event })).toBe(true);
      expect(guard({ event, extra: "ignored", nested: { a: 1 } })).toBe(true);
    });

    it.each(INVALID_PAYLOADS)("rejects %s", (_label, payload) => {
      expect(guard(payload)).toBe(false);
    });

    it("rejects payloads with a different event discriminator", () => {
      // The point of having separate guards is that only the matching
      // one fires. A guard that returns true for the wrong event would
      // cross-wire env progress into the comm stream (or similar).
      const otherEvents = GUARDS.filter(([, , e]) => e !== event).map(([, , e]) => e);
      for (const other of otherEvents) {
        expect(guard({ event: other })).toBe(false);
      }
    });
  });

  it("guards are mutually exclusive for a given payload", () => {
    // A single broadcast must be claimed by at most one guard. If a
    // future refactor accidentally normalizes event names such that
    // two guards pass simultaneously, the first matching subscriber
    // would silently double-handle.
    for (const [, , event] of GUARDS) {
      const payload = { event };
      const hits = GUARDS.filter(([, guard]) => guard(payload));
      expect(hits.length).toBe(1);
    }
  });
});
