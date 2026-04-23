# Telemetry UI and /telemetry page design

**Status:** Draft for review
**Date:** 2026-04-23
**Repos:** `nteract/desktop` + `nteract/nteract.io`

## Summary

Replace the lightly-hidden "Learn more" link on the onboarding telemetry toggle with a real `/telemetry` page on nteract.io, and turn the toggle itself into an affirmation moment rather than a setting. Add a Privacy section to the desktop Settings window so the choice is revisitable without the CLI.

The `/telemetry` page is the primary deliverable. It communicates trust in three registers: a warm letter (voice), a live-looking ping preview (specificity), and a collapsed receipt (precision). The writing frames the heartbeat as a *signal that helps funders see nteract is used*, not as direct funding. Copy calls out NumFOCUS stewardship.

## Problem

Today the onboarding wizard shows a toggle with one line of terse copy and a "Learn more" link pointing at `https://nteract.io/docs/telemetry`, which 404s. The source copy lives in `docs/telemetry.md` but is never published. After onboarding, there is no in-app UI to revisit the setting. The only other way to change it is `runt config telemetry {enable,disable,status}`, which most users won't discover.

The toggle also fights for attention with the Python package-manager picker on the same step. Telemetry shares a moment with an unrelated choice, so the user either clicks through without reading or bounces on a trust signal that wasn't earned.

Three things are broken:

1. **Dead link.** Learn more goes nowhere.
2. **No ongoing surface.** Post-onboarding, the choice is invisible.
3. **Flat framing.** The current copy is accurate but doesn't explain *why* the project asks. NumFOCUS stewardship, grant-funder signal value, and the absence of any personal data are all unspoken.

## Goals

- Ship a `/telemetry` page on nteract.io that earns trust and opts people in.
- Recast the onboarding disclosure as an affirmation, not a footer.
- Add a Settings → Privacy pane on desktop so the choice is revisitable and auditable.
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
6. **Opt-out block.** Three ways, each a small card: onboarding toggle, Settings → Privacy (new), CLI (`runt config telemetry disable`). Each card has a one-line explanation. Environment variable (`NTERACT_TELEMETRY_DISABLE=1`) sits below as a footnote for deployment operators.
7. **Closing line.** One sentence on where to read the source: `crates/runtimed-client/src/telemetry.rs` with a link to GitHub.
8. **Footer rule + NumFOCUS mark.** Small wordmark / text link acknowledging NumFOCUS.

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

The telemetry disclosure moves from a muted strip at the bottom of the Python Environment step to its own visual beat. Same step, more presence:

- Full-width card above the Get Started button, not below the package manager grid.
- Uses a subtle cream-themed background (`--cream-elevated`) rather than `bg-muted/50`.
- Copy: "One anonymous ping per day, so funders can see nteract is used. No names, no paths, no data about your notebooks. [Learn more]"
- The Learn more link opens `https://nteract.io/telemetry` in the browser (Tauri `shell.open`).
- Toggle is the same shadcn switch component used elsewhere, not a custom button. Accessibility wins: real `<button role="switch">` via Radix.

Visually it reads as an affirmation. The user sees the disclosure as part of getting started, not as a legal footer.

**Settings → Privacy pane:**

New section in `apps/notebook/settings/App.tsx`, placed between Appearance and New Notebooks. Renders:

- The same toggle as onboarding (shared component).
- The same one-liner copy.
- Below the toggle, four small links:
  - "Read the full telemetry page" → https://nteract.io/telemetry
  - "View install ID" → shows current `install_id` and a button to rotate (deletes current, regenerates on next ping).
  - "Last ping" → shows the most recent `last_sent_at` timestamp for app / daemon / mcp, pulled from the same settings doc.
  - "Run `runt config telemetry status`" → copyable inline command.

The install-ID rotation is the one new bit of functionality. It's a privacy affordance: if a user wants a fresh identity, they can reset without deleting the whole settings file. Implementation: add a `rotate_install_id` Tauri command that writes a new UUIDv4 via the daemon sync client, same path as `set_synced_setting`.

**Shared toggle component:**

Extract the onboarding telemetry disclosure into a single shared React component, `TelemetryDisclosure`, used by both onboarding and the Settings pane. Two props: `variant: 'onboarding' | 'settings'` and `className?`. Keeps copy and behavior in one place, which is the long-term win for "make sure both surfaces never drift."

**docs/telemetry.md:**

Replace the file with a pointer to the published page plus a short developer-facing section (code paths, test strategy, how to add new fields). The long-form user-facing content moves to the published page as the canonical home. Rationale: LLMs and agents fetch the markdown sibling of `/telemetry` directly, so there's no reason to maintain two separate copies.

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

### Data flow (unchanged but worth stating)

```
User toggles onboarding or Settings switch
        │
        ▼
invoke("set_synced_setting", { key: "telemetry_enabled", value: bool })
        │
        ▼
Tauri command → daemon SyncClient → Automerge doc
        │
        ▼
~/.config/nteract/settings.json mirror on disk
        │
        ▼
telemetry::should_send() reads at next heartbeat tick → send or skip
```

No new backend logic. The Privacy pane reads the same `SyncedSettings` struct and writes through the same command.

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

- Snapshot test of the shared `TelemetryDisclosure` component in both variants.
- Integration test (existing Tauri test harness, if available): toggling from Settings writes the correct value, and a heartbeat skip is observed after flipping off.
- The existing CI-enforced `cargo test -p runtimed --test tokio_mutex_lint` stays green; no new locks.

## Alternatives considered

- **Put the page under `/docs/telemetry`.** Matches the dead link literally. Rejected: we don't have a `/docs` hierarchy today, and adding one for a single page is scope creep. `/telemetry` is simpler and the link will be updated wherever it appears anyway.
- **Publish as a blog post.** Trivially supported by existing MDX machinery. Rejected: the page is evergreen reference, not an announcement.
- **Sidebar always-visible, no accordions.** Richer layout, less cognitive load. Rejected after the visual preview: makes mobile messy and compresses the letter too hard. Desktop keeps the pinned ping preview but hides the full receipt behind accordions.
- **Live install counter.** Considered for the hero area. Rejected: commits the project to an ops burden (a public endpoint, cache strategy, trust in the number) for a pledge-drive beat you explicitly said not to chase.

## Open questions

- **Install-ID rotation semantics.** Proposed: rotation also clears all three (app / daemon / mcp) `last_sent_at` markers so the new ID isn't throttled by the 20-hour cooldown. Alternative: preserve throttle state so a rotating user can't amplify their own ping count. Lean is "clear the markers" because the 60 req/min Cloudflare rate limit is the real defense and a user who rotates daily still produces one ping per day per source.
- **NumFOCUS attribution:** Exact wording of the footer acknowledgement. The spec says "NumFOCUS wordmark + short text." Open to a stronger attribution if there's a sponsored-project template.
- **When to mention the email address / contact.** The page currently has no "email us with privacy concerns" line. Low-lift to add if desired. Not added by default.

## Rollout plan

Two PRs, one per repo. Each structured as a sequence of reviewable commits.

1. **PR 1 (nteract.io):** Ship `/telemetry`.
   - Commit A: Add Source Serif 4 as `--font-page-serif`, add the `.cream-page` palette scope.
   - Commit B: Scaffold `app/telemetry/page.tsx` + `app/telemetry/raw.md/route.ts`, wire middleware + vercel.json + llms.txt + sitemap.
   - Commit C: Complete page content, typography pass, accordion interactions.
   - Commit D: `lib/telemetry-data.ts` typed module and its unit tests.
2. **PR 2 (desktop):** Ship the desktop touchpoints.
   - Commit A: Extract `TelemetryDisclosure` shared component (no behavior change).
   - Commit B: Rework the onboarding step to use `TelemetryDisclosure` in the affirmation layout; fix the Learn more URL to `/telemetry`.
   - Commit C: Add the Settings → Privacy pane. Wires the shared toggle, install-ID view, last-ping display, link to the page.
   - Commit D: `rotate_install_id` Tauri command + daemon plumbing, with the rotate-and-clear-markers semantics.
   - Commit E: Slim `docs/telemetry.md` to the developer-facing residue.

PR 2 depends on PR 1 being live so the Learn more link resolves. Within each PR the commits are ordered so any single commit lands a coherent slice; reviewers can read the diff one commit at a time.

## Follow-ups parked

- **Cream-style markdown / prose redesign.** Extend `--font-page-serif` and the cream palette into a broader prose treatment used for data-science blog posts and documentation pages. Its own PR on nteract.io. Reuses the tokens introduced here.
- **Space Grotesk remains the display face** for security posts and engineering content. Explicit split: Source Serif for long-form reading / data science, Space Grotesk for systems / release / security posts. This split is a follow-up design note, not a code change.
- **Rust-to-TS data check.** If `lib/telemetry-data.ts` drifts from the Rust source, add a CI job that parses the emitter's field enum and diffs against the TS module. Probably not needed until the field set actually changes.
