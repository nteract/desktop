# Telemetry Desktop Touchpoints Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the pre-checked telemetry toggle in onboarding with an explicit two-button choice at the end of the flow, add a Settings → Privacy pane so the decision is revisitable, introduce an `install_id` rotation primitive, and gate heartbeats behind a new `telemetry_consent_recorded` flag so no ping fires until the user has pressed one of the two buttons.

**Architecture:** Add `telemetry_consent_recorded: bool` to the daemon's `SyncedSettings` struct and extend `should_send()` / `blocking_gates()` in `crates/runtimed-client/src/telemetry.rs` to require it. Backfill the flag from `onboarding_completed` so existing users keep implicit consent. Frontend-side: a new shared `TelemetryDisclosureCard` React component renders the "One anonymous daily ping" card, reused from both onboarding and Settings. Onboarding replaces its old toggle + single "Get Started" button with a disclosure card + primary "You can count on me!" + secondary "Opt out of metrics, continue" CTAs. A new `rotate_install_id` Tauri command writes a fresh UUIDv4 and clears all three `last_sent_at` markers in one atomic operation.

**Tech Stack:** Rust (tokio, serde, ts-rs), React + TypeScript (Vite), Tauri, shadcn/Radix, Automerge sync client.

**Repo:** `/Users/kylekelley/projects/desktop` (main branch)

**Spec:** `docs/superpowers/specs/2026-04-23-telemetry-ui-and-docs-design.md`

**Assumes:** PR 1 from the nteract/telemetry repo is merged so the `/v1/install/:id` endpoint exists. PR 2 from nteract/nteract.io is live so `https://nteract.io/telemetry` resolves. These are external dependencies, not blockers for local development. If you're implementing this before those ship, the Learn more link on onboarding will 404 until they do.

---

## File Structure

Files created:

- `apps/notebook/src/components/TelemetryDisclosureCard.tsx`: shared disclosure card (eyebrow + body + Learn more link).
- `apps/notebook/settings/sections/Privacy.tsx`: new Privacy section rendered inside the Settings window.
- `apps/notebook/src/components/TelemetryDisclosureCard.test.tsx`: snapshot test.

Files modified:

- `crates/runtimed-client/src/settings_doc.rs`: add `telemetry_consent_recorded` field; update default + backfill migration.
- `crates/runtimed-client/src/telemetry.rs`: extend `should_send()` and `blocking_gates()` to require `telemetry_consent_recorded`; add a `rotate_install_id_and_clear_markers` helper.
- `src/bindings/SyncedSettings.ts`: regenerated, includes the new field.
- `crates/notebook/src/lib.rs`: add `rotate_install_id` Tauri command; register it.
- `apps/notebook/onboarding/App.tsx`: replace the toggle with the two-button CTA and shared disclosure card.
- `src/hooks/useSyncedSettings.ts`: expose `telemetryEnabled`, `setTelemetryEnabled`, `installId`, `rotateInstallId`, `lastPingTimes` (for the Privacy pane).
- `apps/notebook/settings/App.tsx`: render the new Privacy section between Appearance and New Notebooks.
- `crates/runt/src/main.rs`: extend `runt config telemetry status` output to surface `telemetry_consent_recorded` (so the CLI matches the GUI).
- `docs/telemetry.md`: slim to a developer-facing readme that points at `https://nteract.io/telemetry`.

Each frontend file has one clear responsibility. The Settings sections directory is new (`apps/notebook/settings/sections/`); follow-up privacy-adjacent sections (future erase button, audit export, etc.) slot in here too.

---

## Task 1: Add telemetry_consent_recorded to SyncedSettings

**Files:**
- Modify: `crates/runtimed-client/src/settings_doc.rs`

This is the gate that makes the new onboarding UX work. The default is `false`, meaning a fresh install *cannot* emit a heartbeat until the user has pressed one of the onboarding buttons.

- [ ] **Step 1: Add the field to the struct**

Find the telemetry block near the end of `SyncedSettings` (right below `install_id`) and add the new field:

```rust
    /// Opaque per-install UUIDv4. Generated on first heartbeat, persisted in
    /// settings. Not derived from any identifying data.
    #[serde(default)]
    pub install_id: String,

    /// Master telemetry switch. When false, no heartbeat pings are sent.
    #[serde(default = "default_telemetry_enabled")]
    pub telemetry_enabled: bool,

    /// Whether the user has explicitly recorded a telemetry decision (pressed
    /// either the "You can count on me!" or "Opt out of metrics, continue"
    /// button during onboarding). Default false. Until this is true, no
    /// heartbeat fires, even when `telemetry_enabled = true`. Satisfies the
    /// GDPR "clear affirmative action" requirement.
    #[serde(default)]
    pub telemetry_consent_recorded: bool,
```

- [ ] **Step 2: Update `Default`**

In `impl Default for SyncedSettings`, add the new field after `telemetry_enabled`:

```rust
            install_id: String::new(),
            telemetry_enabled: true,
            telemetry_consent_recorded: false,
            telemetry_last_daemon_ping_at: None,
```

- [ ] **Step 3: Backfill migration for existing users**

Add a function near the bottom of the file:

```rust
/// Backfill `telemetry_consent_recorded` for installations that completed
/// onboarding before the consent flag existed. Without this, all existing
/// users would look like they had never consented, and their heartbeats
/// would stop at the next app launch.
///
/// Called once on daemon startup. Idempotent.
pub fn backfill_telemetry_consent(settings: &mut SyncedSettings) {
    if !settings.telemetry_consent_recorded && settings.onboarding_completed {
        settings.telemetry_consent_recorded = true;
    }
}
```

- [ ] **Step 4: Run cargo check**

```bash
cargo check -p runtimed-client
```

Expected: success, no warnings.

- [ ] **Step 5: Regenerate TypeScript bindings**

```bash
cargo test -p runtimed-client --features ts-bindings
```

Expected: success. Verify `src/bindings/SyncedSettings.ts` now contains `telemetry_consent_recorded: boolean`.

- [ ] **Step 6: Commit**

```bash
git add crates/runtimed-client/src/settings_doc.rs src/bindings/SyncedSettings.ts
git commit -m "feat(settings): add telemetry_consent_recorded field with backfill"
```

---

## Task 2: Gate heartbeats behind the consent flag (TDD)

**Files:**
- Modify: `crates/runtimed-client/src/telemetry.rs`

The goal is: `should_send()` returns `false` when `telemetry_consent_recorded = false`, even if everything else is green.

- [ ] **Step 1: Write the failing test**

Append to the `mod tests` block in `crates/runtimed-client/src/telemetry.rs`:

```rust
    #[test]
    fn test_should_send_requires_consent_recorded() {
        // Everything green except consent_recorded = false.
        assert!(!should_send_full(true, true, false, None, 1000));
    }

    #[test]
    fn test_should_send_with_all_true() {
        assert!(should_send_full(true, true, true, None, 1000));
    }

    #[test]
    fn test_blocking_gates_consent_not_recorded() {
        let gates = blocking_gates_full(true, true, false, None, 1000);
        assert!(gates.contains(&"consent not recorded"));
    }
```

- [ ] **Step 2: Run the tests to confirm they fail**

```bash
cargo test -p runtimed-client telemetry::tests::test_should_send_requires_consent_recorded telemetry::tests::test_should_send_with_all_true telemetry::tests::test_blocking_gates_consent_not_recorded
```

Expected: compile error ("cannot find function `should_send_full`" etc.).

- [ ] **Step 3: Add the new signatures, keep the old ones as thin wrappers**

Above the existing `should_send`:

```rust
pub fn should_send_full(
    telemetry_enabled: bool,
    onboarding_completed: bool,
    consent_recorded: bool,
    last_ping_at: Option<u64>,
    now_secs: u64,
) -> bool {
    if !consent_recorded {
        return false;
    }
    should_send(telemetry_enabled, onboarding_completed, last_ping_at, now_secs)
}

pub fn blocking_gates_full(
    telemetry_enabled: bool,
    onboarding_completed: bool,
    consent_recorded: bool,
    last_ping_at: Option<u64>,
    now_secs: u64,
) -> Vec<&'static str> {
    let mut gates = blocking_gates(telemetry_enabled, onboarding_completed, last_ping_at, now_secs);
    if !consent_recorded {
        gates.push("consent not recorded");
    }
    gates
}
```

Rationale: keeping `should_send` / `blocking_gates` unchanged avoids a cascade of caller updates. The `_full` variants are what `try_send` calls.

- [ ] **Step 4: Update `try_send` to use `should_send_full`**

Change:

```rust
    if !should_send(
        settings.telemetry_enabled,
        settings.onboarding_completed,
        last_ping_at,
        now,
    ) {
        return;
    }
```

To:

```rust
    if !should_send_full(
        settings.telemetry_enabled,
        settings.onboarding_completed,
        settings.telemetry_consent_recorded,
        last_ping_at,
        now,
    ) {
        return;
    }
```

- [ ] **Step 5: Run the tests to confirm they pass**

```bash
cargo test -p runtimed-client telemetry::tests
```

Expected: PASS, including existing tests (which exercise the old `should_send` unchanged).

- [ ] **Step 6: Commit**

```bash
git add crates/runtimed-client/src/telemetry.rs
git commit -m "feat(telemetry): gate heartbeats behind telemetry_consent_recorded"
```

---

## Task 3: Add rotate_install_id_and_clear_markers helper (TDD)

**Files:**
- Modify: `crates/runtimed-client/src/telemetry.rs`

This is the Rust side of the install-ID rotation. It takes a mutable settings doc, rotates the ID, and clears the three `last_sent_at` timestamps. Pure function so it's testable without a daemon.

- [ ] **Step 1: Write the failing test**

Append to `mod tests`:

```rust
    use crate::settings_doc::SyncedSettings;

    #[test]
    fn test_rotate_install_id_changes_id_and_clears_markers() {
        let mut s = SyncedSettings::default();
        s.install_id = "abc".to_string();
        s.telemetry_last_daemon_ping_at = Some(111);
        s.telemetry_last_app_ping_at = Some(222);
        s.telemetry_last_mcp_ping_at = Some(333);

        let new_id = rotate_install_id_in(&mut s);

        assert_ne!(new_id, "abc");
        assert_eq!(s.install_id, new_id);
        assert!(uuid::Uuid::parse_str(&new_id).is_ok());
        assert_eq!(s.telemetry_last_daemon_ping_at, None);
        assert_eq!(s.telemetry_last_app_ping_at, None);
        assert_eq!(s.telemetry_last_mcp_ping_at, None);
    }
```

- [ ] **Step 2: Run the test to confirm it fails**

```bash
cargo test -p runtimed-client telemetry::tests::test_rotate_install_id_changes_id_and_clears_markers
```

Expected: compile error ("cannot find function `rotate_install_id_in`").

- [ ] **Step 3: Implement the helper**

Add above `mod tests`:

```rust
/// Rotate the install ID to a fresh UUIDv4 and clear all three `last_sent_at`
/// markers. Callers persist the mutated settings via the daemon sync client.
///
/// Clearing markers prevents the 20-hour throttle from silently suppressing
/// the first ping under the new ID. The 60 req/min rate limit at the
/// Cloudflare edge is the defense against rotation abuse.
pub fn rotate_install_id_in(settings: &mut crate::settings_doc::SyncedSettings) -> String {
    let new_id = uuid::Uuid::new_v4().to_string();
    settings.install_id = new_id.clone();
    settings.telemetry_last_daemon_ping_at = None;
    settings.telemetry_last_app_ping_at = None;
    settings.telemetry_last_mcp_ping_at = None;
    new_id
}
```

- [ ] **Step 4: Run the test to confirm it passes**

```bash
cargo test -p runtimed-client telemetry::tests::test_rotate_install_id_changes_id_and_clears_markers
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/runtimed-client/src/telemetry.rs
git commit -m "feat(telemetry): add rotate_install_id_in helper"
```

---

## Task 4: Add rotate_install_id Tauri command

**Files:**
- Modify: `crates/notebook/src/lib.rs`

The frontend invokes this. The command reads current settings from the daemon, calls `rotate_install_id_in`, and writes the four modified fields back.

- [ ] **Step 1: Locate the existing `set_synced_setting` command**

Find `async fn set_synced_setting(...)` (around line 3321). The new command goes right below it.

- [ ] **Step 2: Add the new command**

```rust
/// Rotate the install ID to a fresh UUIDv4 and clear all three `last_sent_at`
/// markers. Used by the Privacy pane for user-initiated identity reset.
///
/// Returns the new install ID so the UI can display it without another round-trip.
#[tauri::command]
async fn rotate_install_id() -> Result<String, String> {
    let socket_path = runt_workspace::default_socket_path();
    let mut client = runtimed::sync_client::SyncClient::connect_with_timeout(
        socket_path,
        std::time::Duration::from_millis(500),
    )
    .await
    .map_err(|e| format!("Daemon unavailable: {}. Install ID not rotated.", e))?;

    let new_id = uuid::Uuid::new_v4().to_string();

    // Write the four fields atomically as separate put_value calls. The sync
    // client is serial per connection, and the daemon does not interleave
    // puts within a single client session.
    client
        .put_value(
            "install_id",
            &serde_json::Value::String(new_id.clone()),
        )
        .await
        .map_err(|e| format!("sync error (install_id): {}", e))?;
    client
        .put_value("telemetry_last_daemon_ping_at", &serde_json::Value::Null)
        .await
        .map_err(|e| format!("sync error (daemon marker): {}", e))?;
    client
        .put_value("telemetry_last_app_ping_at", &serde_json::Value::Null)
        .await
        .map_err(|e| format!("sync error (app marker): {}", e))?;
    client
        .put_value("telemetry_last_mcp_ping_at", &serde_json::Value::Null)
        .await
        .map_err(|e| format!("sync error (mcp marker): {}", e))?;

    Ok(new_id)
}
```

- [ ] **Step 3: Register the command in the Tauri builder**

Find the existing `.invoke_handler(tauri::generate_handler![` list (search for `complete_onboarding,`). Add `rotate_install_id,` next to it:

```rust
            complete_onboarding,
            rotate_install_id,
```

- [ ] **Step 4: Build to verify**

```bash
cargo build -p notebook
```

Expected: success.

- [ ] **Step 5: Commit**

```bash
git add crates/notebook/src/lib.rs
git commit -m "feat(tauri): add rotate_install_id command"
```

---

## Task 5: Extend useSyncedSettings with telemetry fields

**Files:**
- Modify: `src/hooks/useSyncedSettings.ts`

Expose the telemetry settings the Privacy pane and onboarding need.

- [ ] **Step 1: Add state hooks inside `useSyncedSettings`**

Find the existing state declarations near the top of the hook (around the `keepAliveSecs` declaration). Below them, add:

```ts
  const [telemetryEnabled, setTelemetryEnabledState] = useState<boolean>(true);
  const [telemetryConsentRecorded, setTelemetryConsentRecordedState] =
    useState<boolean>(false);
  const [installId, setInstallIdState] = useState<string>("");
  const [lastDaemonPingAt, setLastDaemonPingAtState] = useState<number | null>(
    null,
  );
  const [lastAppPingAt, setLastAppPingAtState] = useState<number | null>(null);
  const [lastMcpPingAt, setLastMcpPingAtState] = useState<number | null>(null);
```

- [ ] **Step 2: Read them from the daemon on mount**

Find the `invoke<SyncedSettings>("get_synced_settings").then((settings) => { ... })` block. Add these reads inside the `.then` callback, after the existing block:

```ts
        if (typeof settings.telemetry_enabled === "boolean") {
          setTelemetryEnabledState(settings.telemetry_enabled);
        }
        if (typeof settings.telemetry_consent_recorded === "boolean") {
          setTelemetryConsentRecordedState(settings.telemetry_consent_recorded);
        }
        if (typeof settings.install_id === "string") {
          setInstallIdState(settings.install_id);
        }
        if (typeof settings.telemetry_last_daemon_ping_at === "number") {
          setLastDaemonPingAtState(settings.telemetry_last_daemon_ping_at);
        } else if (typeof settings.telemetry_last_daemon_ping_at === "bigint") {
          setLastDaemonPingAtState(Number(settings.telemetry_last_daemon_ping_at));
        }
        if (typeof settings.telemetry_last_app_ping_at === "number") {
          setLastAppPingAtState(settings.telemetry_last_app_ping_at);
        } else if (typeof settings.telemetry_last_app_ping_at === "bigint") {
          setLastAppPingAtState(Number(settings.telemetry_last_app_ping_at));
        }
        if (typeof settings.telemetry_last_mcp_ping_at === "number") {
          setLastMcpPingAtState(settings.telemetry_last_mcp_ping_at);
        } else if (typeof settings.telemetry_last_mcp_ping_at === "bigint") {
          setLastMcpPingAtState(Number(settings.telemetry_last_mcp_ping_at));
        }
```

- [ ] **Step 3: Mirror the reads in the `settings:changed` listener**

Find the `listen<SyncedSettings>("settings:changed", (event) => { ... })` block. After its existing reads, add:

```ts
      if (typeof event.payload.telemetry_enabled === "boolean") {
        setTelemetryEnabledState(event.payload.telemetry_enabled);
      }
      if (typeof event.payload.telemetry_consent_recorded === "boolean") {
        setTelemetryConsentRecordedState(event.payload.telemetry_consent_recorded);
      }
      if (typeof event.payload.install_id === "string") {
        setInstallIdState(event.payload.install_id);
      }
```

(Last-ping timestamps change rarely and are only fetched on mount; no listener wiring needed.)

- [ ] **Step 4: Add the setters**

Below the existing `setFeatureFlag` declaration, add:

```ts
  const setTelemetryEnabled = useCallback((value: boolean) => {
    setTelemetryEnabledState(value);
    invoke("set_synced_setting", {
      key: "telemetry_enabled",
      value,
    }).catch((e) =>
      console.warn("[settings] Failed to persist telemetry_enabled:", e),
    );
  }, []);

  const setTelemetryConsentRecorded = useCallback((value: boolean) => {
    setTelemetryConsentRecordedState(value);
    invoke("set_synced_setting", {
      key: "telemetry_consent_recorded",
      value,
    }).catch((e) =>
      console.warn("[settings] Failed to persist telemetry_consent_recorded:", e),
    );
  }, []);

  const rotateInstallId = useCallback(async (): Promise<string | null> => {
    try {
      const newId = await invoke<string>("rotate_install_id");
      setInstallIdState(newId);
      setLastDaemonPingAtState(null);
      setLastAppPingAtState(null);
      setLastMcpPingAtState(null);
      return newId;
    } catch (e) {
      console.warn("[settings] Failed to rotate install_id:", e);
      return null;
    }
  }, []);
```

- [ ] **Step 5: Expose the new fields from the return statement**

Add these entries to the returned object (preserve the existing file's grouping of related fields together):

```ts
    telemetryEnabled,
    setTelemetryEnabled,
    telemetryConsentRecorded,
    setTelemetryConsentRecorded,
    installId,
    rotateInstallId,
    lastDaemonPingAt,
    lastAppPingAt,
    lastMcpPingAt,
```

- [ ] **Step 6: Type-check**

```bash
cd apps/notebook && pnpm exec tsc --noEmit
```

Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add src/hooks/useSyncedSettings.ts
git commit -m "feat(hooks): expose telemetry state + rotateInstallId to UI"
```

---

## Task 6: Build the shared TelemetryDisclosureCard component

**Files:**
- Create: `apps/notebook/src/components/TelemetryDisclosureCard.tsx`
- Create: `apps/notebook/src/components/TelemetryDisclosureCard.test.tsx`

The same card is rendered in both onboarding and the Settings Privacy pane. Copy lives once.

- [ ] **Step 1: Write the test**

```tsx
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { TelemetryDisclosureCard } from "./TelemetryDisclosureCard";

describe("TelemetryDisclosureCard", () => {
  it("renders the eyebrow, body, and a Learn more link", () => {
    render(<TelemetryDisclosureCard />);
    expect(screen.getByText(/One anonymous daily ping/i)).toBeInTheDocument();
    expect(screen.getByText(/Version, platform, architecture/i)).toBeInTheDocument();
    const link = screen.getByRole("link", { name: /read the full details/i });
    expect(link).toHaveAttribute("href", "https://nteract.io/telemetry");
  });
});
```

- [ ] **Step 2: Run the test (expect failure because the component doesn't exist yet)**

```bash
cd apps/notebook && pnpm exec vitest run src/components/TelemetryDisclosureCard.test.tsx
```

Expected: FAIL. Module not found.

- [ ] **Step 3: Write the component**

```tsx
import { open } from "@tauri-apps/plugin-shell";
import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

interface TelemetryDisclosureCardProps {
  className?: string;
  footer?: ReactNode;
}

/**
 * One-source-of-truth disclosure card for telemetry.
 *
 * Rendered in:
 *  - Onboarding (step 2, above the two consent buttons).
 *  - Settings → Privacy (alongside the revisit toggle).
 *
 * The Learn more link opens https://nteract.io/telemetry in the system
 * browser via Tauri's shell plugin. The page is the canonical place for
 * "What is sent / never sent / retention / rights". This card only
 * carries the minimum disclosure.
 */
export function TelemetryDisclosureCard({
  className,
  footer,
}: TelemetryDisclosureCardProps) {
  const handleOpenLearnMore = async (e: React.MouseEvent) => {
    e.preventDefault();
    try {
      await open("https://nteract.io/telemetry");
    } catch {
      // Tauri not available (unit test); href provides a fallback.
    }
  };

  return (
    <div
      className={cn(
        "rounded-lg border p-4 bg-muted/40 space-y-2",
        className,
      )}
    >
      <div className="text-[10px] uppercase tracking-[0.14em] text-primary/80 font-semibold">
        One anonymous daily ping
      </div>
      <p className="text-sm text-foreground leading-6">
        Version, platform, architecture. No names, no paths, nothing about your
        notebooks.
      </p>
      <a
        href="https://nteract.io/telemetry"
        onClick={handleOpenLearnMore}
        className="inline-block text-xs text-primary underline hover:text-foreground"
      >
        Read the full details
      </a>
      {footer ? <div className="pt-1">{footer}</div> : null}
    </div>
  );
}
```

- [ ] **Step 4: Run the test again**

```bash
cd apps/notebook && pnpm exec vitest run src/components/TelemetryDisclosureCard.test.tsx
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/notebook/src/components/TelemetryDisclosureCard.tsx apps/notebook/src/components/TelemetryDisclosureCard.test.tsx
git commit -m "feat(components): add shared TelemetryDisclosureCard"
```

---

## Task 7: Rework the onboarding step to use the two-button CTA

**Files:**
- Modify: `apps/notebook/onboarding/App.tsx`

Replace the pre-checked toggle + single "Get Started" button with a disclosure card + "You can count on me!" (primary) + "Opt out of metrics, continue" (secondary).

- [ ] **Step 1: Remove the old toggle**

Delete the `const [telemetryEnabled, setTelemetryEnabled] = useState(true);` line (around line 163).

Delete the entire `{/* Telemetry disclosure */}` block (lines ~436–469), which renders the muted strip.

Delete the old `{/* Get Started button */}` block (lines ~471–480).

- [ ] **Step 2: Add a shared CTA handler**

Near `handleGetStarted` (around line 280), add a new state:

```tsx
  const [isSubmitting, setIsSubmitting] = useState(false);
```

Rename `handleGetStarted` to `handleChoice` and have it accept a `telemetryEnabled` argument:

```tsx
  // Save settings + consent, then complete onboarding.
  const handleChoice = useCallback(
    async (telemetryEnabled: boolean) => {
      if (!runtime || !pythonEnv) return;
      if (!daemonReady || !poolReady) return;
      if (isSubmitting) return;
      setIsSubmitting(true);

      try {
        await invoke("set_synced_setting", {
          key: "default_runtime",
          value: runtime,
        });
        await invoke("set_synced_setting", {
          key: "default_python_env",
          value: pythonEnv,
        });
        await invoke("set_synced_setting", {
          key: "telemetry_enabled",
          value: telemetryEnabled,
        });
        await invoke("set_synced_setting", {
          key: "telemetry_consent_recorded",
          value: true,
        });
        await invoke("set_synced_setting", {
          key: "onboarding_completed",
          value: true,
        });

        setSetupComplete(true);

        try {
          await invoke("complete_onboarding", {
            defaultRuntime: runtime,
            defaultPythonEnv: pythonEnv,
          });
          // Window closes itself on success.
        } catch (completeError) {
          console.error("Failed to complete onboarding:", completeError);
          setSetupComplete(false);
          setIsSubmitting(false);
          setErrorMessage("Failed to create notebook window. Please try again.");
        }
      } catch (e) {
        console.error("Failed to save onboarding settings:", e);
        setIsSubmitting(false);
        setErrorMessage("Failed to save settings. Please try again.");
      }
    },
    [daemonReady, poolReady, runtime, pythonEnv, isSubmitting],
  );
```

Update `handleSkip` for the daemon-failure fallback so it also records consent (opt-out):

```tsx
  const handleSkip = useCallback(async () => {
    await invoke("set_synced_setting", {
      key: "telemetry_enabled",
      value: false,
    });
    await invoke("set_synced_setting", {
      key: "telemetry_consent_recorded",
      value: true,
    });
    await invoke("complete_onboarding", {
      defaultRuntime: runtime ?? "python",
      defaultPythonEnv: pythonEnv ?? "uv",
    });
  }, [runtime, pythonEnv]);
```

- [ ] **Step 3: Import the shared card**

Add at the top of the file:

```tsx
import { TelemetryDisclosureCard } from "@/components/TelemetryDisclosureCard";
```

- [ ] **Step 4: Insert the disclosure card + two buttons in place of the removed blocks**

After the `PageDots` + spacer block (around line 432) and inside the `page === 2` fragment, add:

```tsx
            {/* Telemetry decision: replaces the old pre-checked toggle. */}
            <div className="space-y-3">
              <TelemetryDisclosureCard />

              <Button
                onClick={() => handleChoice(true)}
                disabled={!canProceed || isSubmitting}
                className="w-full"
                size="lg"
              >
                {setupComplete
                  ? "All set!"
                  : canProceed
                    ? "You can count on me!"
                    : pythonEnv === null
                      ? "Select a package manager"
                      : "Setting up..."}
              </Button>

              <button
                type="button"
                onClick={() => handleChoice(false)}
                disabled={!canProceed || isSubmitting}
                className="w-full text-xs text-muted-foreground underline hover:text-foreground disabled:opacity-50 py-1"
              >
                Opt out of metrics, continue
              </button>
            </div>

            {/* Continue anyway (daemon failure fallback) */}
            {daemonFailed && !setupComplete && (
              <Button onClick={handleSkip} variant="ghost" className="w-full" size="sm">
                Continue anyway
              </Button>
            )}
```

- [ ] **Step 5: Type-check and dev-preview**

```bash
cd apps/notebook && pnpm exec tsc --noEmit
```

Expected: clean.

To preview visually, the human will need to run the app from their terminal (see CLAUDE.md: don't launch the GUI from the agent session). Leave this as a manual verification step in the PR description.

- [ ] **Step 6: Commit**

```bash
git add apps/notebook/onboarding/App.tsx
git commit -m "feat(onboarding): replace telemetry toggle with explicit consent buttons"
```

---

## Task 8: Build the Settings → Privacy pane

**Files:**
- Create: `apps/notebook/settings/sections/Privacy.tsx`
- Modify: `apps/notebook/settings/App.tsx`

The section renders the disclosure card + a switch + install ID viewer + rotation button + last-ping display + "Erase my data" mailto link.

- [ ] **Step 1: Create `apps/notebook/settings/sections/Privacy.tsx`**

```tsx
import { open } from "@tauri-apps/plugin-shell";
import { useCallback, useState } from "react";
import { TelemetryDisclosureCard } from "@/components/TelemetryDisclosureCard";
import { Switch } from "@/components/ui/switch";

interface PrivacySectionProps {
  telemetryEnabled: boolean;
  onTelemetryChange: (value: boolean) => void;
  installId: string;
  onRotate: () => Promise<string | null>;
  lastDaemonPingAt: number | null;
  lastAppPingAt: number | null;
  lastMcpPingAt: number | null;
}

function formatRelative(secs: number | null): string {
  if (secs === null) return "never";
  const now = Math.floor(Date.now() / 1000);
  const diff = now - secs;
  if (diff < 60) return `${diff}s ago`;
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  return `${Math.floor(diff / 86400)}d ago`;
}

export function PrivacySection({
  telemetryEnabled,
  onTelemetryChange,
  installId,
  onRotate,
  lastDaemonPingAt,
  lastAppPingAt,
  lastMcpPingAt,
}: PrivacySectionProps) {
  const [isRotating, setIsRotating] = useState(false);

  const handleRotate = useCallback(async () => {
    if (isRotating) return;
    const ok = window.confirm(
      "Rotate your install ID? This generates a new random identifier. Your old rows on the server become unlinkable and age out at 400 days.",
    );
    if (!ok) return;
    setIsRotating(true);
    try {
      await onRotate();
    } finally {
      setIsRotating(false);
    }
  }, [isRotating, onRotate]);

  const handleEraseMyData = useCallback(async () => {
    const subject = encodeURIComponent("Telemetry erasure request");
    const body = encodeURIComponent(
      `Please erase all telemetry rows for install ID: ${installId}\n\nThanks.`,
    );
    const url = `mailto:privacy@nteract.io?subject=${subject}&body=${body}`;
    try {
      await open(url);
    } catch {
      window.location.href = url;
    }
  }, [installId]);

  const handleReadPage = useCallback(async () => {
    try {
      await open("https://nteract.io/telemetry");
    } catch {
      /* ignore */
    }
  }, []);

  return (
    <div className="space-y-3">
      <span className="text-xs font-semibold text-muted-foreground uppercase tracking-wider">
        Privacy
      </span>

      <TelemetryDisclosureCard
        footer={
          <div className="flex items-center justify-between pt-1">
            <span className="text-xs text-muted-foreground">
              Send anonymous daily ping
            </span>
            <Switch
              checked={telemetryEnabled}
              onCheckedChange={onTelemetryChange}
            />
          </div>
        }
      />

      <div className="space-y-1.5">
        <div className="flex items-center justify-between gap-3">
          <span className="text-xs text-muted-foreground shrink-0">
            Install ID
          </span>
          <code
            className="text-[11px] text-foreground truncate bg-muted/50 px-2 py-0.5 rounded"
            title={installId}
          >
            {installId || "(not yet set)"}
          </code>
          <button
            type="button"
            onClick={handleRotate}
            disabled={isRotating || !installId}
            className="text-xs text-primary underline hover:text-foreground disabled:opacity-50"
          >
            {isRotating ? "Rotating..." : "Rotate"}
          </button>
        </div>

        <div className="flex items-center justify-between">
          <span className="text-xs text-muted-foreground">Last ping</span>
          <span className="text-xs text-foreground tabular-nums">
            app {formatRelative(lastAppPingAt)} · daemon{" "}
            {formatRelative(lastDaemonPingAt)} · mcp{" "}
            {formatRelative(lastMcpPingAt)}
          </span>
        </div>
      </div>

      <div className="flex flex-wrap gap-3 text-xs pt-1">
        <button
          type="button"
          onClick={handleReadPage}
          className="text-primary underline hover:text-foreground"
        >
          Read the full telemetry page
        </button>
        <button
          type="button"
          onClick={handleEraseMyData}
          disabled={!installId}
          className="text-primary underline hover:text-foreground disabled:opacity-50"
        >
          Erase my data
        </button>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Wire it into `apps/notebook/settings/App.tsx`**

Add the import near the top:

```tsx
import { PrivacySection } from "./sections/Privacy";
```

In the `useSyncedSettings()` destructure inside `export default function App()`, add:

```tsx
  const {
    defaultRuntime,
    setDefaultRuntime,
    defaultPythonEnv,
    setDefaultPythonEnv,
    defaultUvPackages,
    setDefaultUvPackages,
    defaultCondaPackages,
    setDefaultCondaPackages,
    defaultPixiPackages,
    setDefaultPixiPackages,
    keepAliveSecs,
    setKeepAliveSecs,
    featureFlags,
    setFeatureFlag,
    telemetryEnabled,
    setTelemetryEnabled,
    installId,
    rotateInstallId,
    lastDaemonPingAt,
    lastAppPingAt,
    lastMcpPingAt,
  } = useSyncedSettings();
```

Find the rendered `Appearance` section's closing tag (look for where the Flavor div ends, around line 290 of the existing file). Immediately after the Appearance block's outermost `</div>`, insert:

```tsx
        <PrivacySection
          telemetryEnabled={telemetryEnabled}
          onTelemetryChange={setTelemetryEnabled}
          installId={installId}
          onRotate={rotateInstallId}
          lastDaemonPingAt={lastDaemonPingAt}
          lastAppPingAt={lastAppPingAt}
          lastMcpPingAt={lastMcpPingAt}
        />
```

- [ ] **Step 3: Type-check**

```bash
cd apps/notebook && pnpm exec tsc --noEmit
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add apps/notebook/settings/sections/Privacy.tsx apps/notebook/settings/App.tsx
git commit -m "feat(settings): add Privacy section with rotation + mailto erase"
```

---

## Task 9: Apply the consent backfill on daemon startup

**Files:**
- Find-and-modify: the daemon's settings-load path. Likely `crates/runtimed/src/lib.rs` or `crates/runtimed/src/settings.rs`.

The struct field and the backfill function exist; now we need to *call* the backfill once when the daemon starts.

- [ ] **Step 1: Find the daemon's settings load site**

```bash
grep -rn "SyncedSettings" crates/runtimed/src | grep -v '//' | head
```

Look for where the daemon reads settings into a `SyncedSettings` (e.g., at startup, or in a `load_synced_settings()` helper).

- [ ] **Step 2: Call `backfill_telemetry_consent` after the load**

Wherever the daemon has a parsed `SyncedSettings` it intends to write back, insert:

```rust
runtimed_client::settings_doc::backfill_telemetry_consent(&mut settings);
```

Persist the updated settings via the existing write path (the daemon's sync-backed write, same one used for `put_value`).

If no obvious central load site exists, an alternative pattern is:

- In the first `try_send` call inside `telemetry.rs`, if `onboarding_completed && !telemetry_consent_recorded`, call `write_setting("telemetry_consent_recorded", true)` before proceeding. This is lazier but avoids a daemon-code change.

- [ ] **Step 3: Verify with a test**

Write a unit test in `crates/runtimed-client/src/settings_doc.rs` (if one doesn't already cover this):

```rust
    #[test]
    fn test_backfill_telemetry_consent_flips_for_onboarded_users() {
        let mut s = SyncedSettings::default();
        s.onboarding_completed = true;
        s.telemetry_consent_recorded = false;
        backfill_telemetry_consent(&mut s);
        assert!(s.telemetry_consent_recorded);
    }

    #[test]
    fn test_backfill_telemetry_consent_noop_for_fresh_installs() {
        let mut s = SyncedSettings::default();
        // onboarding_completed = false by default
        backfill_telemetry_consent(&mut s);
        assert!(!s.telemetry_consent_recorded);
    }
```

Place in the existing `mod tests` (if present) or add a fresh one at the end of the file.

- [ ] **Step 4: Run the tests**

```bash
cargo test -p runtimed-client settings_doc::tests
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/runtimed-client/src/settings_doc.rs crates/runtimed/src/
git commit -m "feat(daemon): backfill telemetry_consent_recorded for onboarded users"
```

---

## Task 10: Extend `runt config telemetry status` output

**Files:**
- Modify: `crates/runt/src/main.rs`

The CLI command already exists. We only add `consent_recorded` to the output so the CLI matches the GUI.

- [ ] **Step 1: Find the existing `Status` handler**

```bash
grep -n "TelemetryCommands::Status" crates/runt/src/main.rs
```

It'll print a block that includes `telemetry_enabled`, `install_id`, and last-ping timestamps.

- [ ] **Step 2: Add a line for `consent_recorded`**

Right after the line that prints `telemetry_enabled`:

```rust
    println!(
        "  consent_recorded: {}",
        settings.telemetry_consent_recorded
    );
```

(Match the formatting of adjacent `println!` calls exactly so indentation and alignment are consistent.)

- [ ] **Step 3: Build + smoke test**

```bash
cargo build -p runt-cli
./target/debug/runt config telemetry status
```

Expected: the output now includes `consent_recorded: true` (or false, depending on state).

- [ ] **Step 4: Commit**

```bash
git add crates/runt/src/main.rs
git commit -m "feat(runt): surface consent_recorded in telemetry status"
```

---

## Task 11: Slim docs/telemetry.md

**Files:**
- Modify: `docs/telemetry.md`

User-facing content now lives on nteract.io/telemetry. This file becomes a developer's readme.

- [ ] **Step 1: Replace the file with the slim version**

```markdown
# Telemetry (developer notes)

User-facing copy lives at https://nteract.io/telemetry. This file is for developers.

## Files

| Path | Purpose |
|------|---------|
| `crates/runtimed-client/src/telemetry.rs` | Heartbeat emitter. `should_send_full`, `blocking_gates_full`, `try_send`, `heartbeat_loop`, `heartbeat_once`. |
| `crates/runtimed-client/src/settings_doc.rs` | `SyncedSettings` fields: `install_id`, `telemetry_enabled`, `telemetry_consent_recorded`, three `telemetry_last_*_ping_at` fields. `backfill_telemetry_consent` migration. |
| `crates/runt/src/main.rs` | `runt config telemetry {status,enable,disable}`. |
| `apps/notebook/src/components/TelemetryDisclosureCard.tsx` | Shared disclosure card. |
| `apps/notebook/onboarding/App.tsx` | "You can count on me!" / "Opt out of metrics, continue" buttons. |
| `apps/notebook/settings/sections/Privacy.tsx` | Revisit UI with install ID rotation. |

## Adding a heartbeat field

1. Add the field to `HeartbeatPayload` in `crates/runtimed-client/src/telemetry.rs`.
2. Update `lib/telemetry-data.ts` on nteract.io so the page and raw.md export include it.
3. Update `docs/telemetry-schema.md` in the nteract/telemetry repo.
4. Add a migration to `worker/migrations/` for the new column + enum constraint.
5. Extend `worker/src/ingest.ts` to accept and persist the new field.

## The consent gate

The `telemetry_consent_recorded` flag is the explicit-consent gate added after
the onboarding redesign. Until it is true, `should_send_full` returns false
even when `telemetry_enabled = true`. The daemon sets it on startup via
`backfill_telemetry_consent` for users who completed onboarding before the flag
existed.

## Suppression

- `RUNTIMED_DEV=1` or `RUNTIMED_WORKSPACE_PATH` is set.
- `CI` is set.
- `NTERACT_TELEMETRY_DISABLE=1` is set.
- `telemetry_enabled = false` or `telemetry_consent_recorded = false` in settings.
- Platform or arch is unsupported.
- Last ping for this source was less than 20 hours ago.

## Testing

```
cargo test -p runtimed-client telemetry::tests
cargo test -p runtimed-client settings_doc::tests
cd apps/notebook && pnpm exec vitest run src/components/TelemetryDisclosureCard.test.tsx
```

## Endpoints

Ingest: `POST https://telemetry.runtimed.com/v1/ping` (source: `nteract/telemetry`).
Erasure: `DELETE https://telemetry.runtimed.com/v1/install/:install_id`.
```

- [ ] **Step 2: Commit**

```bash
git add docs/telemetry.md
git commit -m "docs(telemetry): slim to developer-facing readme; user copy now on nteract.io"
```

---

## Task 12: Full verification pass

**Files:**
- None, just verification.

- [ ] **Step 1: Run the full Rust test suite for the touched crates**

```bash
cargo test -p runtimed-client
cargo test -p notebook
```

Expected: all green.

- [ ] **Step 2: Run the tokio-mutex lint (invariant in CLAUDE.md)**

```bash
cargo test -p runtimed --test tokio_mutex_lint
```

Expected: green.

- [ ] **Step 3: Run the frontend checks**

```bash
cd apps/notebook && pnpm exec tsc --noEmit
cd apps/notebook && pnpm exec vitest run
```

Expected: clean + all green.

- [ ] **Step 4: Lint pass**

```bash
cargo xtask lint
```

Expected: clean. If not, `cargo xtask lint --fix` and re-commit.

- [ ] **Step 5: Manual UX check (for the human reviewer, not the agent)**

Note in the PR description that the agent did NOT launch the app to test onboarding visually, per CLAUDE.md. Reviewers should:

- Launch with `cargo xtask notebook` from a fresh settings state (back up `~/.config/nteract/settings.json` and remove it).
- Walk through onboarding, confirm the two-button CTA, verify either button ends onboarding and sets the right values.
- Open Settings, confirm the Privacy pane renders and the rotation button generates a new UUID.
- Confirm the Learn more link opens `https://nteract.io/telemetry` in the system browser.

---

## Self-Review Summary

- **`telemetry_consent_recorded` field + backfill:** Task 1, reinforced in Task 9.
- **Consent gate in `should_send` / `blocking_gates`:** Task 2.
- **Install-ID rotation (pure function + Tauri command):** Tasks 3, 4.
- **Hook exposes new state:** Task 5.
- **Shared disclosure card:** Task 6.
- **Onboarding two-button CTA:** Task 7.
- **Settings Privacy pane:** Task 8.
- **CLI reflects consent_recorded:** Task 10.
- **docs/telemetry.md slimmed:** Task 11.
- **Verification:** Task 12.

All spec requirements for the desktop deliverable (PR 3 in the spec's rollout) are covered. The plan assumes the telemetry and nteract.io PRs have shipped; if they haven't, the erasure and Learn more links are simply not yet live, which is fine for local testing.
