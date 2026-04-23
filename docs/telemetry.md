# Telemetry (developer notes)

User-facing copy lives at https://nteract.io/telemetry. This file is for developers working on the desktop codebase.

## Files

| Path | Purpose |
|------|---------|
| `crates/runtimed-client/src/telemetry.rs` | Heartbeat emitter. `should_send_full`, `blocking_gates_full`, `try_send`, `heartbeat_loop`, `heartbeat_once`, `rotate_install_id_in`. |
| `crates/runtimed-client/src/settings_doc.rs` | `SyncedSettings` fields: `install_id`, `telemetry_enabled`, `telemetry_consent_recorded`, three `telemetry_last_*_ping_at` fields. `backfill_telemetry_consent` / `backfill_telemetry_consent_in_doc` migrations. |
| `crates/runt/src/main.rs` | `runt config telemetry {status,enable,disable}`. |
| `src/components/TelemetryDisclosureCard.tsx` | Shared disclosure card, used in onboarding and Settings. |
| `apps/notebook/onboarding/App.tsx` | "You can count on me!" / "Opt out of metrics, continue" buttons. |
| `apps/notebook/settings/sections/Privacy.tsx` | Revisit pane with install ID rotation. |

## The consent gate

`telemetry_consent_recorded` is the explicit-consent gate added after the onboarding redesign. Until it is `true`, `should_send_full` returns `false` even when `telemetry_enabled = true`.

Daemon startup runs `backfill_telemetry_consent_in_doc` so existing users who completed onboarding before the flag existed keep sending heartbeats — they've already indicated they're okay with it. Fresh installs stay at `false` until the user presses either CTA on the onboarding screen.

## Install ID rotation

`rotate_install_id_in` generates a fresh UUIDv4 and clears the three per-source `last_ping_at` timestamps so the 20-hour throttle doesn't silently suppress the first ping under the new ID. The Tauri command `rotate_install_id` wraps it and is invoked from Settings → Privacy.

## Suppression

Heartbeats are suppressed when any of these hold:

- `RUNTIMED_DEV=1` or `RUNTIMED_WORKSPACE_PATH` is set
- `CI` is set
- `NTERACT_TELEMETRY_DISABLE=1` is set
- `telemetry_enabled = false` in settings
- `telemetry_consent_recorded = false` in settings
- `onboarding_completed = false`
- Platform or arch is unsupported
- Last ping for this source was less than 20 hours ago

## Adding a heartbeat field

Keep in mind every field added here ships to users as part of their ping payload. Add with care, and update:

1. `HeartbeatPayload` in `crates/runtimed-client/src/telemetry.rs`.
2. The public page at `https://nteract.io/telemetry` so what's documented matches what's sent.

The server side is separate infrastructure and versions independently — coordinate the rollout so the server accepts the new field before the client emits it.

## Testing

```
cargo test -p runtimed-client telemetry::tests
cargo test -p runtimed-client settings_doc::tests
pnpm vp test run src/components/__tests__/TelemetryDisclosureCard
```

## Endpoints

- Ingest: `POST https://telemetry.runtimed.com/v1/ping`

## Visual preview

Run `pnpm --dir apps/notebook dev` and open `http://localhost:5174/gallery/` to preview `TelemetryDisclosureCard`, the onboarding CTA block, and Settings → Privacy without launching the desktop app.
