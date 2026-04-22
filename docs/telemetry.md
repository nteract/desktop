# Telemetry

nteract collects anonymous daily usage data to understand how many people use the app. This document describes exactly what is sent, what is not, and how to opt out.

## What is sent

Every heartbeat ping carries six fields:

| Field | Example | Description |
|---|---|---|
| `install_id` | `550e8400-e29b-41d4-a716-446655440000` | Opaque UUIDv4 generated on first run. Not derived from any identifying data. |
| `source` | `app`, `daemon`, `mcp` | Which process sent the ping. |
| `version` | `2.2.1` | Release version. |
| `channel` | `stable` or `nightly` | Release channel. |
| `platform` | `macos`, `linux`, `windows` | OS family. |
| `arch` | `arm64`, `x86_64` | CPU architecture. |

The server adds `received_at` (unix timestamp) on its side.

## What is never sent or stored

- Hostname, username, home directory, or any filesystem path
- Notebook contents, cell outputs, kernel names, or environment details
- Dependency names or versions (Python, Node, R, system packages)
- Hardware identifiers (MAC address, serial number, disk UUID)
- Client IP address at rest - Cloudflare observes it briefly for rate limiting; the database does not store it
- `User-Agent` or any HTTP header beyond `Content-Type`

## How it works

Three processes send heartbeats independently:

- **app** - fires once per launch of the desktop GUI
- **daemon** - checks hourly, sends if >20 hours since last ping
- **mcp** - checks hourly, sends if >20 hours since last ping

All pings go to `https://telemetry.runtimed.com/v1/ping` as a JSON POST. The endpoint enforces a 60 req/min rate limit per IP at the Cloudflare edge.

## Emission gates

A heartbeat is suppressed if any of these conditions hold:

| Gate | Trigger |
|---|---|
| Dev mode | `RUNTIMED_DEV=1` or `RUNTIMED_WORKSPACE_PATH` is set |
| CI | `CI` environment variable is set |
| Kill switch | `NTERACT_TELEMETRY_DISABLE` environment variable is set |
| Disabled | `telemetry_enabled = false` in settings |
| Not onboarded | `onboarding_completed = false` (fresh install before first-run screen) |
| Unsupported host | Platform or architecture not in the server's enum |
| Throttled | Last ping for this source was less than 20 hours ago |

## Opting out

Three ways to disable telemetry, all equivalent:

1. **Onboarding toggle** - the first-run screen includes a telemetry toggle (default: on). Flip it off before clicking "Get Started".

2. **CLI** - run `runt config telemetry disable`. Check status with `runt config telemetry status`.

3. **Environment variable** - set `NTERACT_TELEMETRY_DISABLE=1`. This is an emergency kill switch for locked-down deployments or CI images.

There is no server-side delete endpoint. When you disable telemetry the client stops sending pings. Existing data ages out under the retention policy below.

## Retention

- **Raw pings**: kept for 400 days, then deleted by a nightly cleanup job.
- **Daily aggregate counts**: kept indefinitely. These contain no `install_id` - only counts of distinct installs grouped by day, source, version, channel, platform, and arch.

## Schema evolution

New fields may be added over time (additive only). Any field removal is a breaking change that gets a new route version (`/v2/ping`).
