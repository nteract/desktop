# Branding & Naming: Moving to the nteract Org

Evaluation of what it means to move the desktop notebook app (and by extension, the daemon and CLI) from the `runtimed` GitHub org to the `nteract` org.

## Current Branding Inventory

Here's every place a brand name is baked in today:

### Binaries & Crate Names

| Crate | Package Name | Binary Name | Location |
|-------|-------------|-------------|----------|
| CLI | `runt-cli` | `runt` | `crates/runt/` |
| Daemon | `runtimed` | `runtimed` | `crates/runtimed/` |
| Desktop app | `notebook` | `notebook` | `crates/notebook/` |
| Sidecar | `sidecar` | `sidecar` | `crates/sidecar/` |

### App Identity & Bundling

| Field | Current Value | File |
|-------|--------------|------|
| Tauri `productName` | `runt-notebook` | `crates/notebook/tauri.conf.json` |
| Tauri `identifier` | `com.runtimed.notebook` | `crates/notebook/tauri.conf.json` |
| Bundled binaries | `binaries/runtimed`, `binaries/runt` | `crates/notebook/tauri.conf.json` |

### System Services

| Platform | Service ID | Config Path |
|----------|-----------|-------------|
| macOS (launchd) | `io.runtimed` | `~/Library/LaunchAgents/io.runtimed.plist` |
| Linux (systemd) | `runtimed.service` | `~/.config/systemd/user/runtimed.service` |

### Filesystem Paths (User-Facing)

All of these live on end-user machines:

```
~/.cache/runt/                         # Linux cache root
~/Library/Caches/runt/                 # macOS cache root
  ├── runtimed.sock
  ├── daemon.lock
  ├── daemon.json
  ├── runtimed.log
  ├── envs/
  ├── conda-envs/
  └── worktrees/{hash}/

~/.config/runt/                        # Linux config root
~/Library/Application Support/runt/    # macOS config root
  ├── trust-key
  └── bin/runtimed                     # Installed daemon binary

~/Library/Application Support/runt-notebook/
  └── settings.json                    # macOS settings
~/.config/runt-notebook/
  └── settings.json                    # Linux settings
```

### Package Distribution

| Channel | Name | Notes |
|---------|------|-------|
| PyPI | `runtimed` | Ships the `runt` binary via maturin |
| Curl installer | `curl https://i.safia.sh/runtimed/runt \| sh` | Custom domain |
| GitHub Releases | `runtimed/runtimed` | Library crates + binaries |
| crates.io | `runtimelib`, `jupyter-protocol`, `nbformat` | Published under `runtimed` org |

### GitHub References

| Location | URL |
|----------|-----|
| Workspace `Cargo.toml` | `https://github.com/runtimed/runt` |
| All crate `Cargo.toml` | `https://github.com/runtimed/runt` |
| README | `https://github.com/runtimed/runtimed` (library repo) |
| Issue/PR links in source | `https://github.com/runtimed/runt/issues/*` |

### npm / Frontend

| Field | Value | File |
|-------|-------|------|
| Root `package.json` name | `runtimed` | `package.json` |
| Notebook app package name | `notebook-ui` | `apps/notebook/package.json` |
| Component registry | `@nteract` scope | `components.json` |

---

## The nteract Connection Today

The project already has a meaningful relationship with nteract:

- **UI components** are pulled from the `@nteract/elements` registry via shadcn
- There's a full contributing guide (`contributing/nteract-elements.md`) for upstreaming changes
- The output renderers (ANSI, markdown, HTML, JSON) all come from nteract/elements
- The BSD-3-Clause license is compatible with nteract's licensing

Critically, **the maintainers are the same people**. Kyle Kelley maintains both the runtimed org and the nteract org. This isn't an acquisition or a donation to a different team — it's consolidating work under the org with more Jupyter ecosystem recognition. The runtimed org was the starting point; nteract is the natural home now that the project has grown from a set of Rust libraries into a full-featured desktop notebook app.

This also explains the naming lineage: `runtimed` started as a GitHub org for Rust-native Jupyter runtime libraries (`runtimelib`, `jupyter-protocol`). The daemon inherited the org name. Then the notebook app grew around it. The org name was always about the runtime layer — nteract is a better umbrella for the user-facing product.

---

## Naming Strategy Options

### Option A: Keep "Runt" as the Product Name Under nteract

Move the repo to `nteract/runt`. Keep the CLI as `runt`, the daemon as `runtimed`, and the desktop app as "Runt" or "Runt Notebook."

**Pros:**
- Minimal code changes — binary names, filesystem paths, and service IDs stay the same
- Existing users (and their `~/.cache/runt/` directories) aren't disrupted
- "runt" is short, memorable, and already established
- The PyPI package `runtimed` doesn't need to change
- Clear product identity separate from the org

**Cons:**
- The `runtimed` org name now lives under a different org, which is a bit odd ("nteract/runt" but `pip install runtimed`)
- People may not immediately associate "runt" with nteract
- The `io.runtimed` service identifier and `com.runtimed.notebook` bundle ID reference an org that no longer owns the repo

**Migration scope:** Update `repository` URLs in Cargo.toml, GitHub links in docs, and the curl installer URL. That's about it.

### Option B: Rebrand to "nteract" Fully

Rename everything — the app becomes "nteract", the CLI becomes `nteract`, the daemon becomes `nteractd` or `nteract-daemon`.

**Pros:**
- Strong brand unity — one name, one org, one identity
- nteract has existing name recognition in the Jupyter ecosystem
- Clean slate: `io.nteract.notebook`, `com.nteract.notebook`, `~/.cache/nteract/`

**Cons:**
- Massive migration surface (see inventory above). Every path, service ID, binary name, and package name changes
- Breaks all existing installations — service IDs change, config directories move, users must re-trust notebooks
- The PyPI name `nteract` may already be taken or contentious
- `nteract` as a CLI name is 7 characters vs 4 — minor but real for a tool you type often
- Existing nteract packages on npm (`@nteract/*`) are a different thing (the elements registry) — could cause confusion between the desktop app and the component library

**Migration scope:** Essentially a complete rename. Hundreds of files, user-facing paths, service registrations, package names, and distribution channels.

### Option C: Hybrid — "nteract" Brand, "runt" Tools

The desktop app is marketed as "nteract" (or "nteract Notebook"), but the CLI stays `runt`, the daemon stays `runtimed`, and internal identifiers keep the `runt`/`runtimed` names. The GitHub repo moves to `nteract/runt`.

**Pros:**
- Users see "nteract" in the app name and marketing — benefits from the brand
- Developers and power users keep the short `runt` CLI they're used to
- Service IDs, filesystem paths, and package names don't change
- The Tauri `productName` can change to "nteract" without touching anything else
- Avoids collision with existing `@nteract/*` npm scope (those are components, this is the app)

**Cons:**
- Two names to explain: "It's nteract, but you type `runt` in the terminal"
- `com.runtimed.notebook` bundle ID still references the old org
- Documentation needs to reconcile both names

**Migration scope:** Moderate. Update Tauri `productName`, bundle identifier (to `com.nteract.notebook` or `io.nteract.notebook`), repo URLs, and marketing/docs. Binary names and filesystem paths stay.

---

## What Actually Has to Change (Any Option)

Regardless of naming strategy, moving to the nteract org requires:

1. **Repository URLs** — All `Cargo.toml` `repository` fields, doc links, issue references
2. **Curl installer** — `https://i.safia.sh/runtimed/runt` needs a new path or domain
3. **GitHub Release URLs** — Anything referencing `runtimed/runtimed/releases`
4. **CI/CD** — Workflows, secrets, and deployment targets move to the new org

These are mechanical changes that apply no matter what you name things.

---

## What's Harder to Change Later

Some identifiers are sticky — changing them after release is painful:

- **macOS bundle ID** (`com.runtimed.notebook`) — Changing this means existing users get a "new" app. Keychain entries, file associations, and Gatekeeper approvals reset. Do this now if you're going to.
- **launchd service ID** (`io.runtimed`) — Changing requires an uninstall/reinstall cycle for every user. Their daemon won't auto-update across the rename.
- **PyPI package name** (`runtimed`) — Renaming on PyPI is not straightforward. You'd need a new package and a deprecation notice on the old one.
- **Filesystem paths** (`~/.cache/runt/`, `~/.config/runt/`) — Users accumulate state here (trust keys, environments, settings). A migration path is possible but adds complexity.

---

## Recommendation

**Option C (Hybrid)** gives you the best tradeoff. Specifically:

1. **Move the repo** to `nteract/runt`
2. **Update the bundle ID** to `io.nteract.notebook` (do this before any public release — it's the hardest thing to change later)
3. **Update the launchd/systemd service ID** to `io.nteract.runtimed` / `nteract-runtimed.service` (same reason — do it now)
4. **Keep `runt` as the CLI binary** and `runtimed` as the daemon binary
5. **Keep the PyPI package** as `runtimed` (add `nteract` as a keyword)
6. **Keep filesystem paths** as `~/.cache/runt/` and `~/.config/runt/`
7. **Update Tauri `productName`** to "nteract" so the app shows as "nteract" in docks, window titles, etc.

This is a bit funny — the app is "nteract" but the CLI is `runt` — but it's actually a common pattern. Docker Desktop is "Docker Desktop" but the CLI is `docker`. VS Code is "Visual Studio Code" but the CLI is `code`. The product name and the command-line name don't have to match.

The `runtimed` org was the right place to start — it was about the runtime libraries. But the project outgrew that scope. It's a desktop notebook app now, not just a daemon. nteract is where it belongs, and since the maintainers are the same, the move is a reorg, not a handoff.

The library crates (`runtimelib`, `jupyter-protocol`, `nbformat`) can stay under `runtimed` on crates.io — they're general-purpose Jupyter infrastructure and don't need to be branded as nteract. The app and its tooling (`runt`, `runtimed`, the notebook) move to nteract as the product home.
