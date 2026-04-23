# Telemetry UI and /telemetry page design

**Status:** Draft for review
**Date:** 2026-04-23
**Repos:** `nteract/desktop` + `nteract/nteract.io`

## Summary

Replace the lightly-hidden "Learn more" link on the onboarding telemetry toggle with a real `/telemetry` page on nteract.io, replace the pre-checked toggle with an explicit opt-in button at the end of onboarding, and add a Privacy section to the desktop Settings window so the choice is revisitable without the CLI. The explicit button change also brings the consent model in line with a strict GDPR reading (clear affirmative action, not a pre-ticked default).

The `/telemetry` page is the primary deliverable. It communicates trust in three registers: a warm letter (voice), a live-looking ping preview (specificity), and a collapsed receipt (precision). The writing frames the heartbeat as a *signal that helps funders see nteract is used*, not as direct funding. Copy calls out NumFOCUS stewardship and lists the user's rights plainly.

## Problem

Today the onboarding wizard shows a toggle with one line of terse copy and a "Learn more" link pointing at `https://nteract.io/docs/telemetry`, which 404s. The source copy lives in `docs/telemetry.md` but is never published. After onboarding, there is no in-app UI to revisit the setting. The only other way to change it is `runt config telemetry {enable,disable,status}`, which most users won't discover.

The toggle also fights for attention with the Python package-manager picker on the same step. Telemetry shares a moment with an unrelated choice, so the user either clicks through without reading or bounces on a trust signal that wasn't earned.

Separately, the current UI is a pre-checked toggle labeled "Anonymous usage data." Under a strict GDPR reading (Recital 32) consent must be a "clear affirmative action," not a pre-ticked default. OSS projects commonly ship pre-checked toggles and enforcement is spotty, but it's the kind of thing we should just get right while we're rebuilding this flow.

Four things are broken:

1. **Dead link.** Learn more goes nowhere.
2. **No ongoing surface.** Post-onboarding, the choice is invisible.
3. **Flat framing.** The current copy is accurate but doesn't explain *why* the project asks. NumFOCUS stewardship, grant-funder signal value, and the absence of any personal data are all unspoken.
4. **Implicit consent.** Default-on plus a toggle is a pre-ticked box. A strict GDPR reading wants explicit affirmative action.

## Goals

- Ship a `/telemetry` page on nteract.io that earns trust and opts people in.
- Recast the onboarding disclosure as an affirmation, not a footer. The user makes the decision by pressing a button, not by accepting a pre-checked toggle.
- Add a Settings → Privacy pane on desktop so the choice is revisitable and auditable.
- Bring the consent model in line with a strict GDPR reading: explicit opt-in, easy opt-out, user rights plainly listed, a real privacy contact, and a server-side erasure path.
- Codify Source Serif 4 as `--font-page-serif` on nteract.io so future long-form "cream" pages (dataframes, data science content) reuse it. Space Grotesk stays the default display face for security / engineering posts.

## Non-goals

- Redesigning the broader onboarding flow. Only the telemetry strip on step 2 changes.
- Revisiting what the heartbeat contains. The existing six fields are the source of truth for page copy; no payload changes.
- Shipping a cream-style markdown / prose redesign. Same font token gets introduced, but the broader prose styling overhaul is a separate follow-up PR.
- Adding a "see your own pings" dashboard or per-ping log surface.

## Constraints and inputs

- **Existing content:** `docs/telemetry.md` is accurate and already serves as the technical description. The `/telemetry` page is voice + layout on top of that content. The two must not drift.
- **Existing CLI:** `runt config telemetry {enable,disable,status}` is fully implemented in `crates/runt/src/main.rs`. Docs already reference it; the page should too.
- **Existing settings plumbing:** Onboarding writes `telemetry_enabled` via `invoke("set_synced_setting", {...})` (`apps/notebook/onboarding/App.tsx:294-297`). The same Tauri command backs the Settings window, so adding a Privacy pane is wiring, not new infrastructure.
- **Existing visual system:** The desktop app's cream palette lives in `src/styles/cream-theme.css`. The nteract.io site already loads Space Grotesk, Inter, and JetBrains Mono as CSS variables (`app/layout.tsx`). Source Serif 4 is additive.
- **Existing machinery:** nteract.io serves raw-markdown siblings via `middleware.ts` + `vercel.json` (PRs #621, #623, #624). `/telemetry` participates in this; agents and LLMs get the markdown form for free.

## Design

### Repo A, nteract.io: the `/telemetry` page

**Route:** `app/telemetry/page.tsx` (top-level, no route group. Root layout is fine).

**Raw markdown sibling:** `app/telemetry/raw.md/route.ts` emitting the same content as a markdown string. `middleware.ts` gets `/telemetry` added to its matcher and `rewriteTarget()` case. `vercel.json` gets a `Vary: Accept` header. `app/llms.txt/route.ts` lists the page.

**Layout (desktop):**

Two-column, asymmetric. Prose on the left with generous measure (around 60ch). Sidebar on the right with the live-looking ping preview pinned below the fold, plus quick-jump links. Page padding scales with viewport: tight on small laptops, luxurious on wide screens.

**Layout (mobile):**

Single column. Prose first, ping preview inlined after the opening paragraph, accordions below.

**Typography:**

- Display: **Source Serif 4** (new), loaded via `next/font/google`, exposed as `--font-page-serif`. Applied to h1, h2, blockquote, and the opening paragraph.
- Body: **Inter** (existing, `--font-body`). Line height 1.6.
- Mono: **JetBrains Mono** (existing, `--font-mono`) for the ping JSON and code fragments.

**Color:**

Matches the cream aesthetic evolving in the desktop app:
- `--paper: #faf8f3` (background)
- `--ink: #1e1a18` (text)
- `--rule: #d8cec3` (hairlines)
- `--accent: #955f3b` (section marks, links)
- `--muted: #6b6356` (secondary text)

The palette is page-scoped (declared in the page's own CSS module or a scoped `<style>` block) so it doesn't override the rest of the site's dark theme.

**Page content (in order):**

1. **Eyebrow.** `§ Telemetry` in small caps, accent color.
2. **H1.** "A light ping, and why we ask." Source Serif, regular weight, slight italic optional on "why we ask."
3. **Lede (3 short paragraphs).**
   - nteract is maintained under NumFOCUS. What that means for stewardship.
   - One anonymous daily heartbeat. That the signal exists is how small open-source projects stay visible to grant funders. The ping is *evidence of use*, not funding.
   - If you'd rather not, flip it off. No penalty, no degraded features. Sticky in settings.
4. **Ping preview block.** JetBrains Mono, on a slightly darker cream card. The six fields with example values. Annotated with a one-word hover tag per field (e.g., `install_id` → "random on first run").
5. **Receipt accordion (collapsed by default).** Four sections, each a `<details>` element so it works without JS and is accessible:
   - **Exactly what's sent** (maps 1:1 to the "What is sent" table in docs/telemetry.md)
   - **What is never sent or stored** (maps to the same-named section in docs)
   - **When a ping is suppressed** (emission gates table)
   - **Retention and schema evolution** (retention + schema evolution sections)
6. **Opt-out block.** Three ways, each a small card: onboarding button ("opt out of metrics, continue"), Settings → Privacy (new), CLI (`runt config telemetry disable`). Each card has a one-line explanation. Environment variable (`NTERACT_TELEMETRY_DISABLE=1`) sits below as a footnote for deployment operators.
7. **Your rights.** Plain-language, four bullets, each mapped to a mechanism:
   - **Access.** Open Settings → Privacy to see your install ID, last ping times, and current setting.
   - **Rectify.** There's nothing to rectify. The six fields are facts about your build, not profile data.
   - **Erase.** Rotate your install ID from Settings → Privacy. Your old rows become unlinkable and age out at 400 days. To delete them immediately, email the address below with your install ID; we run a `DELETE` against the raw pings table.
   - **Object / withdraw.** Flip the setting off, any time. No penalty, no features lost.
8. **Sponsored project note.** One paragraph: nteract is a NumFOCUS sponsored project. NumFOCUS's own [privacy policy](https://numfocus.org/privacy-policy) covers NumFOCUS services (its website, event registration, etc.), not sponsored projects. This page is how *nteract* handles your data.
9. **Privacy contact.** One line with a mailto link. Proposed address: `privacy@nteract.io`. See open question on contact plumbing.
10. **Closing line.** One sentence on where to read the source: `crates/runtimed-client/src/telemetry.rs` with a link to GitHub, and `nteract/telemetry` for the server-side endpoint.
11. **Footer rule + NumFOCUS mark.** Small wordmark / text link acknowledging NumFOCUS stewardship.

**Copy voice:**

Engineer-to-engineer. Short declaratives. Avoids hyperbole ("we deeply respect"). Avoids marketing cliches. Nothing chest-thumping. Leads with facts, ends with a single warm line. No em-dashes (matches repo convention).

**Canonical home for user-facing copy:**

After rollout, the `/telemetry` page is the single source of truth for what's sent, what's not, gates, retention, and opt-out steps. `docs/telemetry.md` gets slimmed to a developer-facing readme: code paths, how to add a new field, test strategy, where the heartbeat endpoint lives. Anything the user would care about lives on the website and is reachable as markdown via the raw-sibling route.

To keep the technical *data* (the six fields, gate names, retention periods) from drifting in the page source itself, the page imports a typed module:

- `lib/telemetry-data.ts` on nteract.io exports the field list, gate list, and retention policy as typed constants.
- The page renders tables from this module rather than hard-coded Markdown.
- The raw-markdown sibling serializes the same data so LLM clients see exact numbers.

No cross-repo sync is needed. The Rust emitter (`crates/runtimed-client/src/telemetry.rs`) is the functional source of truth; the TS module is a hand-kept mirror for the page. A short contributor note in `docs/telemetry.md` calls out "if you add a heartbeat field, update `lib/telemetry-data.ts` on nteract.io too."

### Repo B, nteract/desktop: the touchpoints

**Onboarding redesign (step 2):**

The telemetry decision moves from a pre-checked toggle at the bottom of the Python Environment step to an **explicit choice at the end of onboarding**. The user presses one of two buttons; that press is both the consent event and the submit event.

Layout (below the package-manager grid, replacing the current Get Started button):

- **Disclosure card** above the buttons. Cream-elevated background, soft border.
  - Eyebrow: "One anonymous daily ping" (small caps, accent color).
  - Body: "Version, platform, architecture. No names, no paths, nothing about your notebooks."
  - Link: "Read the full details" → opens `https://nteract.io/telemetry` via Tauri `shell.open`.
- **Primary CTA (large, dark):** "You can count on me!" → sets `telemetry_enabled = true` and advances.
- **Secondary CTA (small, underlined, muted):** "Opt out of metrics, continue" → sets `telemetry_enabled = false` and advances.

Neither button is pre-selected; the user picks one. This is the explicit affirmative action that satisfies GDPR Recital 32.

Both button handlers write to `telemetry_enabled` via the existing `set_synced_setting` command, then advance onboarding. No new Tauri command needed. The daemon-failure "Continue anyway" path collapses into the secondary CTA (same behavior: continue without telemetry).

**Default before the user chooses:** the Automerge default for `telemetry_enabled` stays `true` (existing behavior in `settings_doc.rs:309-311`), but a new flag `telemetry_consent_recorded: bool` (default `false`) gates whether a ping may fire. `try_send()` in `telemetry.rs` checks the new flag as part of `blocking_gates()`. If a user has never pressed either onboarding button, no ping is sent, even when `telemetry_enabled` is `true` in settings. Pressing either button sets `telemetry_consent_recorded = true`. This preserves the "don't send before onboarding completes" invariant already covered by the `onboarding_completed` gate, but makes consent-bearing explicit and auditable.

**Settings → Privacy pane:**

New section in `apps/notebook/settings/App.tsx`, placed between Appearance and New Notebooks. Renders:

- A shadcn switch bound to `telemetry_enabled`. Unlike onboarding, this is a toggle (the user already expressed consent and is now revisiting it).
- The same disclosure copy as the onboarding card.
- Below the toggle, five small links:
  - "Read the full telemetry page" → https://nteract.io/telemetry
  - "Your install ID" → shows the current `install_id` in monospace; button to rotate (generates a new UUIDv4 and clears all three `last_sent_at` markers).
  - "Erase my data" → opens a confirmation that composes a `mailto:` to the privacy address with the install ID pre-filled. Explains: "We'll run a DELETE against the raw pings table. Aggregates are already anonymized."
  - "Last ping" → most recent `last_sent_at` timestamp for app / daemon / mcp, pulled from the same settings doc.
  - "Run `runt config telemetry status`" → copyable inline command.

Two new Tauri commands:

- `rotate_install_id`: writes a new UUIDv4 + clears all `last_sent_at` markers via the daemon sync client. Same path as `set_synced_setting`.
- No new command needed for "Erase my data"; it's a `mailto:` the user sends themselves. (A future version could POST to a server-side erasure endpoint directly; keeping this as a manual path avoids adding UI for something that's rare and reviewable.)

**Shared component:**

Extract the disclosure card (eyebrow + body + read-full-details link) into a single shared React component, `TelemetryDisclosureCard`, used by both surfaces. Controls live next to it in each surface:

- Onboarding: two buttons underneath (affirmation CTA + opt-out-and-continue).
- Settings: a switch next to it (already consented; now toggling).

One source of truth for copy. If we ever need to change the disclosure wording, there's one place to edit.

**docs/telemetry.md:**

Replace the file with a pointer to the published page plus a short developer-facing section (code paths, test strategy, how to add new fields, the explicit-consent gate). The long-form user-facing content moves to the published page as the canonical home. Rationale: LLMs and agents fetch the markdown sibling of `/telemetry` directly, so there's no reason to maintain two separate copies.

### Repo C, nteract/telemetry: server-side changes

The ingest endpoint at `https://telemetry.runtimed.com/v1/ping` is hosted in `nteract/telemetry` (Cloudflare Worker + D1). Two additions:

**1. Erasure endpoint.** New route `DELETE /v1/install/{install_id}`:

- Deletes all rows in `pings` where `install_id = ?`.
- Aggregates in `daily_rollup` stay untouched (they already contain no `install_id`).
- Unauthenticated. The `install_id` is the capability: only the user holding it can ask for it to be erased. Rate-limited at the Cloudflare edge on the same ruleset as `/v1/ping` (60 req/min per IP) so it can't be used as a DoS primitive against the DB.
- Returns `204 No Content` on success, `204` also if the install ID isn't found (no information leak about whether an ID exists).
- A `Vary: Origin` and CORS headers that permit requests from `https://nteract.io` so the Settings pane *could* call it directly later without a mail flow. Not wired yet.

**2. Privacy schema disclosure update.** `docs/telemetry-schema.md` in the telemetry repo gains:

- A "Your rights" section mirroring the page.
- A "How to request erasure" section listing the endpoint + the email fallback.
- A one-line pointer to `https://nteract.io/telemetry` as the user-facing page.

No DB migration. No new secrets. The Worker adds one route handler and one test.

### Design tokens introduced

**nteract.io.** `--font-page-serif` is set via `next/font/google` in `app/layout.tsx` (same pattern as Space Grotesk, Inter, JetBrains Mono). The cream palette is page-scoped in `app/globals.css`:

```css
.cream-page {
  --paper: #faf8f3;
  --ink: #1e1a18;
  --rule: #d8cec3;
  --accent: #955f3b;
  --muted: #6b6356;
  background: var(--paper);
  color: var(--ink);
  font-family: var(--font-body);
}

.cream-page h1,
.cream-page h2,
.cream-page blockquote {
  font-family: var(--font-page-serif);
}
```

`.cream-page` is the opt-in class for Source-Serif / cream-colored pages. Future pages in the "data science" family add `className="cream-page"` to their root and get the whole treatment.

**desktop: no new tokens.** The redesign reuses existing cream-theme variables from `src/styles/cream-theme.css`.

### Data flow

**Consent recorded at onboarding:**

```
User presses primary or secondary onboarding button
        │
        ▼
invoke("set_synced_setting", { key: "telemetry_enabled", value: bool })
invoke("set_synced_setting", { key: "telemetry_consent_recorded", value: true })
        │
        ▼
Tauri command → daemon SyncClient → Automerge doc
        │
        ▼
~/.config/nteract/settings.json mirror on disk
        │
        ▼
telemetry::should_send() checks `telemetry_enabled && telemetry_consent_recorded` → send or skip
```

**Revisited from Settings:**

Same path, just the switch instead of a button. `telemetry_consent_recorded` stays `true` once set.

**Rotate install ID:**

New Tauri command `rotate_install_id` writes a new UUIDv4 + clears all three `last_sent_at` markers via the daemon sync client. Settings doc adds a helper method on `SyncedSettings` for the two-field update.

### Error states

- **Link cannot open (Tauri shell.open fails):** The onboarding disclosure copies the URL to the clipboard and shows a toast.
- **Daemon unreachable when toggling from Settings:** Falls back to the file-path write (same fallback the onboarding already uses).
- **Install-ID rotation without a running daemon:** Disabled with a tooltip ("Start nteract fully to rotate"). Rotation relies on the daemon's settings doc.

### Testing

**nteract.io:**

- Snapshot test of `/telemetry/raw.md` (exercise markdown sibling machinery).
- Unit test of `lib/telemetry-data.ts` verifies the six fields, gate names, retention periods are the expected constants.
- Lighthouse / accessibility pass: the page uses `<details>` for accordions; tab order, aria-expanded, heading hierarchy all verified.

**desktop:**

- Snapshot test of `TelemetryDisclosureCard`.
- Unit test of `blocking_gates()` in `telemetry.rs` covering the new `telemetry_consent_recorded` gate: enabled=true + consent=false → blocked.
- Integration test (existing Tauri test harness, if available): pressing either onboarding button writes the right pair of values, and a heartbeat skip is observed if consent is false.
- Unit test of `rotate_install_id` command: new UUID, old one gone, all three `last_sent_at` markers cleared.
- The existing CI-enforced `cargo test -p runtimed --test tokio_mutex_lint` stays green; no new locks.

**telemetry (server):**

- Unit test for the `DELETE /v1/install/:id` route: pre-seed pings, hit the endpoint, assert rows gone, assert `daily_rollup` untouched.
- Router test confirming 204 on both hit and miss (no information leak).
- Existing `/v1/ping` ingest tests stay green; no schema change.

## Alternatives considered

- **Put the page under `/docs/telemetry`.** Matches the dead link literally. Rejected: we don't have a `/docs` hierarchy today, and adding one for a single page is scope creep. `/telemetry` is simpler and the link will be updated wherever it appears anyway.
- **Publish as a blog post.** Trivially supported by existing MDX machinery. Rejected: the page is evergreen reference, not an announcement.
- **Sidebar always-visible, no accordions.** Richer layout, less cognitive load. Rejected after the visual preview: makes mobile messy and compresses the letter too hard. Desktop keeps the pinned ping preview but hides the full receipt behind accordions.
- **Live install counter.** Considered for the hero area. Rejected: commits the project to an ops burden (a public endpoint, cache strategy, trust in the number) for a pledge-drive beat you explicitly said not to chase.

## Open questions

- **Install-ID rotation semantics.** Proposed: rotation also clears all three (app / daemon / mcp) `last_sent_at` markers so the new ID isn't throttled by the 20-hour cooldown. Alternative: preserve throttle state so a rotating user can't amplify their own ping count. Lean is "clear the markers" because the 60 req/min Cloudflare rate limit is the real defense and a user who rotates daily still produces one ping per day per source.
- **Privacy contact plumbing.** Spec proposes `privacy@nteract.io`. Needs a mailbox or forwarding rule that an actual maintainer reads. If we don't have that, the Google Group / GitHub private issue flow is a fallback. A `mailto:` link that bounces is worse than no link, so this needs to land before the page does. NumFOCUS's `privacy@numfocus.org` is a possible fallback; on their site the address is rendered inert (`javascript:;`) so confirm deliverability before using it.
- **NumFOCUS attribution:** Exact wording of the footer acknowledgement. The spec says "NumFOCUS wordmark + short text." Open to a stronger attribution if there's a sponsored-project template.
- **Erase-my-data UX.** Spec uses a `mailto:` for v1. A one-click delete button (calls the `DELETE` endpoint directly from the Settings pane) is strictly better UX but adds a confirmation modal and network error paths. Lean: ship v1 as mailto, iterate to in-app delete later.

## Rollout plan

Three PRs, one per repo. Each structured as a sequence of reviewable commits.

1. **PR 1 (nteract/telemetry):** Ship the server-side affordances first, so the desktop and site can point at them.
   - Commit A: Add `DELETE /v1/install/:id` route, router test, ingest tests still green.
   - Commit B: CORS allow-list for `https://nteract.io` on the new route (not yet used, future-proofs the in-app delete button).
   - Commit C: Update `docs/telemetry-schema.md` with rights language, erasure instructions, and pointer to `https://nteract.io/telemetry`.
2. **PR 2 (nteract.io):** Ship `/telemetry`.
   - Commit A: Add Source Serif 4 as `--font-page-serif`, add the `.cream-page` palette scope.
   - Commit B: Scaffold `app/telemetry/page.tsx` + `app/telemetry/raw.md/route.ts`, wire middleware + vercel.json + llms.txt + sitemap.
   - Commit C: Complete page content (ping preview, receipt, rights section, NumFOCUS note, privacy contact), typography pass, accordion interactions.
   - Commit D: `lib/telemetry-data.ts` typed module and its unit tests.
3. **PR 3 (desktop):** Ship the desktop touchpoints.
   - Commit A: Add `telemetry_consent_recorded` field to `SyncedSettings`; extend `blocking_gates()` to require it. Backfill migration: if `onboarding_completed` is true, set `telemetry_consent_recorded = true` (existing users keep their implicit consent; new flows require explicit press).
   - Commit B: Extract `TelemetryDisclosureCard` shared component.
   - Commit C: Rework the onboarding step to use the card + two-button CTA ("You can count on me!" / "Opt out of metrics, continue"). Fix the Learn more URL to `/telemetry`. Remove the old pre-checked toggle.
   - Commit D: Add the Settings → Privacy pane. Wires the switch, install-ID view, last-ping display, erase-my-data mailto, link to the page.
   - Commit E: `rotate_install_id` Tauri command + daemon plumbing, with the rotate-and-clear-markers semantics.
   - Commit F: Slim `docs/telemetry.md` to the developer-facing residue.

PR 1 is independent and deploys first. PR 2 depends on PR 1's schema disclosure update being merged (not deployed); the page links to the schema doc's raw Markdown on GitHub in the meantime, or duplicates the text. PR 3 depends on PR 2 being live so the Learn more link resolves.

## Follow-ups parked

- **In-app erasure button.** Once the mailto flow is in use, replace it with a direct `DELETE` call from the Settings pane using the CORS allow-list already shipped in PR 1. Adds a confirmation modal and basic network error handling.
- **Cream-style markdown / prose redesign.** Extend `--font-page-serif` and the cream palette into a broader prose treatment used for data-science blog posts and documentation pages. Its own PR on nteract.io. Reuses the tokens introduced here.
- **Space Grotesk remains the display face** for security posts and engineering content. Explicit split: Source Serif for long-form reading / data science, Space Grotesk for systems / release / security posts. This split is a follow-up design note, not a code change.
- **Rust-to-TS data check.** If `lib/telemetry-data.ts` drifts from the Rust source, add a CI job that parses the emitter's field enum and diffs against the TS module. Probably not needed until the field set actually changes.
