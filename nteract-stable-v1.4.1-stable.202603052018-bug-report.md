# nteract Stable QA Bug Report

## Build under test
- Release: `v1.4.1-stable.202603052018`
- Platform: Linux x64 (VM)
- Install method:
  - Downloaded release assets from GitHub release tag
  - Installed `nteract-stable-linux-x64.deb` via `dpkg -i`
- Installed package version evidence:
  - `Version: 1.4.1-stable.202603052018`
- Checksum evidence:
  - AppImage: `72213f1efbc86beaf2d1911e4db23d06c1e4665e25e903f217be6455d2543548`
  - DEB: `88ee8bf09499357a827c11f99c65fc7863d769536040f03c9cf6902c6f3009a6`

## Test scope covered
- Onboarding and first-run setup
- Opening notebooks from filesystem and fixtures
- Python kernel start + execute
- UV inline trust flow
- Conda inline trust flow
- Mixed inline deps flow (`4-both-deps.ipynb`)
- Deno runtime execution (`10-deno.ipynb`)
- Project file env detection:
  - `pyproject.toml` (`5-pyproject.ipynb`)
  - `pixi.toml` (`6-pixi.ipynb`)
  - `environment.yml` (`7-environment-yml.ipynb`)
- Rich outputs and error outputs
- Multi-window usage (different notebooks)
- Keyboard shortcuts (`Ctrl+S`, `Ctrl+O`, `Ctrl+F`, zoom shortcuts)
- Kernel control validation on fresh notebook:
  - Run All
  - Restart
  - Interrupt (with long-running cell)
- Global Find validation:
  - source matching
  - output matching
  - next/previous navigation
- File menu sample notebooks flow
- Save flow for untitled notebooks (Save dialog behavior)
- Clone notebook flow (Clone dialog behavior)
- Markdown edit/view rendering toggle
- Settings panel and theme switching

> Note: first-run daemon startup failed in this VM environment until I manually started `runtimed run`, after which most core notebook functionality worked well.

---

## Issues found

## 1) Onboarding can get stuck indefinitely on **“Setting up…”**
- Severity: **High**
- Confidence: **High**

### Reproduction
1. Start app with fresh state (new user / first launch).
2. Select runtime (Python) and Python env (UV).
3. Observe CTA/button text becomes **“Setting up…”**.
4. Wait.

### Expected
- Setup completes, or fails with a clear terminal state and recovery options.
- A timeout/failure state should replace endless waiting.

### Actual
- UI remains in “Setting up…” with no completion.
- User is effectively blocked unless using “Continue anyway”.

### Evidence
- `qa/nteract-stable-v1.4.1-stable.202603052018/screenshots/02-onboarding-stuck-setting-up.webp`
- `qa/nteract-stable-v1.4.1-stable.202603052018/screenshots/03-onboarding-no-progress-indicator.webp`

### Suspected root cause
- Onboarding state depends on daemon readiness + pool readiness and can wait without a hard timeout in this failure path.
- In `apps/notebook/onboarding/App.tsx`, setup progression is gated by `daemonReady` / `poolReady` and can remain stuck in a non-terminal waiting UI when daemon startup fails.

---

## 2) Daemon auto-install failure surfaces as generic runtime unavailability
- Severity: **High**
- Confidence: **High**

### Reproduction
1. First-launch onboarding path attempts daemon install automatically.
2. Continue to notebook window.
3. Runtime banner shows unavailable; retry still fails.

### Expected
- Either successful daemon start, or actionable error details in UI.
- If install fails, UI should expose root cause (not just generic reconnect failure).

### Actual
- App shows generic “Runtime unavailable” / reconnect error.
- Logs show daemon install exit code only.

### Evidence
- Screenshot: `qa/nteract-stable-v1.4.1-stable.202603052018/screenshots/04-runtime-daemon-unavailable-banner.webp`
- Screenshot: `qa/nteract-stable-v1.4.1-stable.202603052018/screenshots/05-retry-connection-error.webp`
- App log:
  - `[startup] runtimed install failed with code Some(1)`
  - `[autolaunch] Daemon sync failed after ...`
- Manual reproduction command:
  - `runtimed install` output: `Failed to start service: Failed to connect to bus: No medium found`

### Suspected root cause
- In `crates/notebook/src/lib.rs` (`ensure_daemon_via_sidecar`), sidecar install failure is reduced to exit code + generic guidance.
- Onboarding/runtime UI does not propagate full stderr details (service manager/DBus failure context) to end user.

---

## 3) Re-opening a file already open in another window has ambiguous UX (silent focus-only behavior)
- Severity: **Medium**
- Confidence: **High**

### Reproduction
1. Open notebook A.
2. Use File → Open and select the same notebook A again.
3. Confirm no visible success/failure feedback.

### Expected
- If single-window-per-file is the intended model, show explicit feedback:
  - toast/dialog (“already open; focusing existing window”), or
  - clear visual indicator that focus switch occurred.

### Actual
- The app focuses the existing window for that file and does not create a second window.
- No explicit message indicates this behavior, so the action can look like a no-op.

### Evidence
- Screenshot: `qa/nteract-stable-v1.4.1-stable.202603052018/screenshots/06-open-same-notebook-no-feedback.webp`
- Screenshot: `qa/nteract-stable-v1.4.1-stable.202603052018/screenshots/20-same-file-open-attempt.webp`
- Screenshot: `qa/nteract-stable-v1.4.1-stable.202603052018/screenshots/21-single-window-per-file-behavior.webp`

### Suspected root cause
- Backend window creation uses deterministic path-derived labels (`notebook-<hash>`), which effectively enforces one window per file path in normal operation.
- UX gap: file-open flow does not surface user-facing messaging for “already open; switched focus”.

---

## 4) Daemon error banner dismissal is window-local, causing cross-window inconsistency
- Severity: **Low**
- Confidence: **Medium**

### Reproduction
1. Open multiple notebook windows while daemon is unavailable.
2. Dismiss error banner in one window.
3. Observe banner remains in other windows.

### Expected
- Either globally synchronized dismissal behavior, or clearer per-window semantics.

### Actual
- Banner state appears inconsistent across windows, which is confusing during multi-window workflows.

### Evidence
- Screenshot: `qa/nteract-stable-v1.4.1-stable.202603052018/screenshots/04-runtime-daemon-unavailable-banner.webp`

### Suspected root cause
- `daemonStatus` is local React state per window in `apps/notebook/src/App.tsx`, with no cross-window synchronization for dismissal.

---

## 5) Onboarding setup step lacks meaningful progress detail
- Severity: **Medium**
- Confidence: **High**

### Reproduction
1. Start onboarding and choose runtime/env.
2. Observe setup phase.

### Expected
- Clear progress state (spinner/progress messages/error transitions).

### Actual
- Static “Setting up...” UX with limited explanatory signal.

### Evidence
- Screenshot: `qa/nteract-stable-v1.4.1-stable.202603052018/screenshots/03-onboarding-no-progress-indicator.webp`

### Suspected root cause
- Onboarding UI in `apps/notebook/onboarding/App.tsx` has limited state granularity for long-running setup and does not surface detailed daemon install errors/progress in the primary CTA area.

---

## 6) Native file picker direct typed absolute path fails for valid notebook path
- Severity: **Medium**
- Confidence: **Medium**

### Reproduction
1. Open File → Open.
2. Type full absolute notebook path directly in location field.
3. Press Enter.

### Expected
- Dialog navigates to file or opens it.

### Actual
- Error dialog appears claiming missing directory for an otherwise valid path.

### Evidence
- Screenshot: `qa/nteract-stable-v1.4.1-stable.202603052018/screenshots/07-file-picker-direct-path-entry.webp`
- Screenshot: `qa/nteract-stable-v1.4.1-stable.202603052018/screenshots/08-file-picker-path-error-dialog.webp`

### Suspected root cause
- Likely interaction between platform-native GTK file chooser behavior and dialog integration layer; may be external/native dialog handling rather than notebook core logic.

---

## 7) ipywidgets do not render after successful cell execution
- Severity: **High**
- Confidence: **High**

### Reproduction
1. Open `notebooks/ipywidgets-demo.ipynb` from a working app instance (kernel can reach Idle).
2. Execute import cell:
   - `import ipywidgets as widgets`
   - `from IPython.display import display`
3. Execute widget cell:
   - `widgets.Text(...)`
4. Wait 15+ seconds.

### Expected
- Widget output area appears below the executed cell with an interactive control (e.g., text input box).

### Actual
- Cell executes, but no widget output is rendered.
- No interactive control appears.

### Evidence
- `qa/nteract-stable-v1.4.1-stable.202603052018/screenshots/37-ipywidgets-open-working-instance.webp`
- `qa/nteract-stable-v1.4.1-stable.202603052018/screenshots/38-ipywidgets-import-cell-executed.webp`
- `qa/nteract-stable-v1.4.1-stable.202603052018/screenshots/39-ipywidgets-text-cell-executed.webp`
- `qa/nteract-stable-v1.4.1-stable.202603052018/screenshots/40-ipywidgets-no-output-after-wait.webp`
- `qa/nteract-stable-v1.4.1-stable.202603052018/screenshots/41-ipywidgets-final-no-render.webp`

### Suspected root cause
- Widget MIME output / comm handshake path is likely broken in the execution pipeline.
- Candidate areas:
  - widget output routing/bridge in `src/components/cell/OutputArea.tsx` (`hasWidgetOutputs`, `CommBridgeManager`, iframe message bridge)
  - widget comm forwarding in `apps/notebook/src/hooks/useDaemonKernel.ts` (comm/comm_sync conversion)
  - widget renderer registration in `apps/notebook/src/App.tsx` (`application/vnd.jupyter.widget-view+json`).

---

## Additional observations (working behavior)
- UV trust dialog and startup worked after daemon was running:
  - `.../09-uv-trust-dialog-working-reference.webp`
- Kernel execution worked:
  - `.../10-kernel-idle-working-reference.webp`
  - `.../11-uv-execution-output-working-reference.webp`
- Rich outputs and error rendering worked:
  - `.../12-rich-output-working-reference.webp`
  - `.../13-error-output-working-reference.webp`
- Deno runtime worked:
  - `.../14-deno-execution-pass.webp`
- Mixed UV+Conda dependency flow worked:
  - `.../15-both-deps-trust-dialog-pass.webp`
  - `.../16-both-deps-execution-pass.webp`
- Project-file environment detection worked:
  - pyproject: `.../17-pyproject-execution-pass.webp`
  - pixi: `.../18-pixi-execution-pass.webp`
  - environment.yml: `.../19-environment-yml-execution-pass.webp`
- Kernel controls worked in fresh notebook validation:
  - Run All initial/second pass:
    - `.../22-run-all-fresh-notebook-initial.webp`
    - `.../23-run-all-first-pass-output.webp`
    - `.../24-run-all-new-cell-added.webp`
    - `.../25-run-all-second-pass-output.webp`
  - Restart:
    - `.../26-restart-pass-idle.webp`
  - Interrupt:
    - `.../27-interrupt-pass-busy-state.webp`
    - `.../28-interrupt-pass-stopped.webp`
- Global Find source+output search and navigation worked:
  - `.../29-global-find-source-and-output-pass.webp`
  - `.../30-global-find-navigation-pass.webp`
- File menu sample notebooks worked:
  - `.../31-sample-menu-open-pass.webp`
  - `.../32-sample-notebook-opened-pass.webp`
- Save and clone dialogs worked:
  - untitled save dialog: `.../33-save-untitled-dialog-pass.webp`
  - clone dialog: `.../34-clone-dialog-pass.webp`
- Markdown edit/view rendering worked:
  - edit mode: `.../35-markdown-edit-mode-pass.webp`
  - rendered mode: `.../36-markdown-render-pass.webp`

## Note on same-notebook multi-window sync testing
- Explicit same-file two-window sync (source/output propagation between two windows on one notebook path) could not be exercised because this build appears to enforce single-window-per-file behavior.

## Secondary unstable behavior observed (separate from confirmed widget-render bug)
- Launching additional app instances with alternate isolated state while another instance is running occasionally produced reconnect errors of the form:
  - `update_source: Cell not found: <cell-id>`
- This appears tied to multi-instance/session-state interactions and was not fully minimized into a separate confirmed defect.

