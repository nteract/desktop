/**
 * CodeMirror 6 extension for rendering text attribution highlights.
 *
 * When remote peers (agents, other humans) edit cell source text, the WASM
 * layer pushes `TextAttribution` ranges via the frame bus. This extension
 * renders those ranges with visual feedback about who wrote what.
 *
 * Two effects are combined:
 *
 *   - **Opacity fade-in** — new text appears ghostly (~20% opacity) and
 *     solidifies to full opacity over ~1 second.
 *
 *   - **Underline sweep** — a colored underline sweeps left-to-right via
 *     CSS animation (`background-size` on a bottom-positioned gradient),
 *     then holds and fades out over the mark's lifetime.
 *
 * Architecture:
 * - `StateEffect<AttributionMark[]>` — dispatched from outside to add highlights
 * - `StateField<TimedMark[]>` — stores active marks with timestamps
 * - `DecorationSet` — built from active marks, rebuilt on tick/prune
 * - `ViewPlugin` — runs a periodic prune loop to fade and remove expired marks
 *
 * CSS `@keyframes` for the underline sweep are injected once at module load.
 * Each tick rebuild uses a negative `animation-delay` equal to the mark's age
 * so the sweep "resumes" at the correct position after element recreation.
 *
 * The hot path is purely imperative — no React. Call
 * `addTextAttributions(view, marks)` to push new highlights into an EditorView.
 */

import {
  type Extension,
  RangeSetBuilder,
  StateEffect,
  StateField,
  type Transaction,
} from "@codemirror/state";
import {
  Decoration,
  type DecorationSet,
  EditorView,
  ViewPlugin,
  type ViewUpdate,
} from "@codemirror/view";

// ── Toggles ──────────────────────────────────────────────────────────
// Flip these to compare effects independently. Vite HMR picks up changes.

/** Show the ghostly → solid text opacity fade-in. */
const ENABLE_FADEIN = false;

/** Show the left-to-right underline sweep. */
const ENABLE_UNDERLINE = true;

// ── Configuration ────────────────────────────────────────────────────

/** How long (ms) a highlight remains fully visible before starting to fade. */
const HOLD_MS = 800;

/** How long (ms) the fade-out transition takes after the hold period. */
const FADE_MS = 1200;

/** Total lifetime of a highlight (hold + fade). */
const TOTAL_MS = HOLD_MS + FADE_MS;

/** How long (ms) the text stays ghostly before starting to solidify. */
const GHOST_MS = 80;

/** How long (ms) the text takes to go from ghostly to fully opaque. */
const FADEIN_MS = 800;

/** Duration (ms) of the underline left-to-right sweep animation. */
const SWEEP_MS = 400;

/** How often (ms) the prune loop runs to update opacity and remove expired marks. */
const TICK_MS = 100;

// ── Inject global CSS keyframes (once) ───────────────────────────────

let keyframesInjected = false;

function injectKeyframes(): void {
  if (keyframesInjected || typeof document === "undefined") return;
  keyframesInjected = true;

  const style = document.createElement("style");
  style.textContent = `
    @keyframes cm-attr-sweep {
      from { background-size: 0% 2px; }
      to { background-size: 100% 2px; }
    }
  `;
  document.head.appendChild(style);
}

// ── Types ────────────────────────────────────────────────────────────

export interface AttributionMark {
  /** Character offset in the document where the highlight starts. */
  from: number;
  /** Character offset in the document where the highlight ends. */
  to: number;
  /** Actor label(s) — used for tooltip and color derivation. */
  actors: string[];
  /** CSS color for the highlight background. If omitted, uses the default. */
  color?: string;
}

/** Internal mark with a creation timestamp for fade calculation. */
interface TimedMark {
  from: number;
  to: number;
  actors: string[];
  /** Pre-parsed "R, G, B" string for rgba(). */
  color: string;
  /** `performance.now()` when this mark was created. */
  createdAt: number;
}

// ── Default color ────────────────────────────────────────────────────

const DEFAULT_COLOR = "59, 130, 246"; // RGB for #3b82f6 (blue-500)

/** Parse a hex color string to an "R, G, B" string for rgba(). */
function hexToRgb(hex: string): string {
  const h = hex.replace("#", "");
  if (h.length < 6) return DEFAULT_COLOR;
  const r = Number.parseInt(h.slice(0, 2), 16);
  const g = Number.parseInt(h.slice(2, 4), 16);
  const b = Number.parseInt(h.slice(4, 6), 16);
  if (Number.isNaN(r) || Number.isNaN(g) || Number.isNaN(b)) return DEFAULT_COLOR;
  return `${r}, ${g}, ${b}`;
}

// ── Curves ───────────────────────────────────────────────────────────

/**
 * Text fade-in: starts ghostly, solidifies quickly.
 * Returns an opacity value (0–1) for the text itself.
 */
function computeTextFadeIn(age: number): number {
  if (age >= GHOST_MS + FADEIN_MS) return 1.0;
  if (age < GHOST_MS) return 0.2; // ghostly entrance
  // Linear fade-in from 0.2 → 1.0
  const progress = (age - GHOST_MS) / FADEIN_MS;
  return 0.2 + 0.8 * progress;
}

/**
 * Underline fade-out: full during hold, linear fade after.
 */
function computeUnderlineOpacity(age: number): number {
  if (age >= TOTAL_MS) return 0;
  if (age < HOLD_MS) return 0.6;
  return 0.6 * (1 - (age - HOLD_MS) / FADE_MS);
}

// ── State effects ────────────────────────────────────────────────────

const addAttributionsEffect = StateEffect.define<AttributionMark[]>();

/**
 * Trigger a decoration rebuild on the next tick (prune/fade cycle).
 * The value is unused — its presence is the signal.
 */
const tickEffect = StateEffect.define<null>();

// ── Shared marks field ───────────────────────────────────────────────

/**
 * Stores the active timed marks. Marks are added via `addAttributionsEffect`
 * and removed when they expire past `TOTAL_MS`.
 *
 * Document changes remap `from`/`to` through the change set so highlights
 * track their text even as the document is edited around them.
 */
const marksField = StateField.define<TimedMark[]>({
  create: () => [],
  update(marks: TimedMark[], tr: Transaction): TimedMark[] {
    let updated = marks;

    // Remap positions through document changes.
    // Marks whose range collapses to empty (fully deleted) are dropped.
    if (tr.docChanged) {
      updated = updated
        .map((m) => ({
          ...m,
          from: tr.changes.mapPos(m.from, 1),
          to: tr.changes.mapPos(m.to, -1),
        }))
        .filter((m) => m.from < m.to);
    }

    // Add new marks from effects.
    for (const effect of tr.effects) {
      if (effect.is(addAttributionsEffect)) {
        const now = performance.now();
        const newMarks: TimedMark[] = effect.value
          .filter((m) => m.from < m.to)
          .map((m) => ({
            from: m.from,
            to: m.to,
            actors: m.actors,
            color: m.color ? hexToRgb(m.color) : DEFAULT_COLOR,
            createdAt: now,
          }));
        updated = [...updated, ...newMarks];
      }

      // On tick, prune expired marks.
      if (effect.is(tickEffect)) {
        const now = performance.now();
        updated = updated.filter((m) => now - m.createdAt < TOTAL_MS);
      }
    }

    return updated;
  },
});

// ── Decoration builder ───────────────────────────────────────────────

/**
 * Build a DecorationSet that combines:
 *   - Opacity fade-in (text starts ghostly, solidifies over ~1s)
 *   - Underline sweep (left-to-right CSS animation, then fades out)
 */
function buildDecorations(marks: TimedMark[]): DecorationSet {
  if (marks.length === 0) return Decoration.none;

  const now = performance.now();
  const builder = new RangeSetBuilder<Decoration>();
  const sorted = [...marks].sort((a, b) => a.from - b.from || a.to - b.to);

  for (const mark of sorted) {
    const age = now - mark.createdAt;
    const textOpacity = computeTextFadeIn(age);
    const underlineAlpha = computeUnderlineOpacity(age);

    // Skip if both effects are done (text fully opaque AND underline gone).
    if (textOpacity >= 0.995 && underlineAlpha <= 0.005) continue;

    // ── Text opacity fade-in ──
    const opacityStyle =
      ENABLE_FADEIN && textOpacity < 0.995 ? `opacity: ${textOpacity.toFixed(3)};` : "";

    // ── Underline sweep (left-to-right via CSS animation) ──
    //
    // Uses a bottom-positioned gradient as a fake underline, animated
    // via @keyframes cm-attr-sweep on background-size. Each tick rebuild
    // recreates the element, so we use a negative animation-delay equal
    // to the mark's age to "resume" the sweep at the correct position.
    //
    // After the sweep completes (age > SWEEP_MS), we skip the animation
    // and just set background-size: 100% 2px directly.
    let underlineStyle = "";
    if (ENABLE_UNDERLINE && underlineAlpha > 0.005) {
      const color = `rgba(${mark.color}, ${underlineAlpha.toFixed(3)})`;
      const bg = `background-image: linear-gradient(${color}, ${color}); background-repeat: no-repeat; background-position: bottom left;`;

      if (age < SWEEP_MS) {
        // Still sweeping — use animation with negative delay to resume.
        underlineStyle = `${bg} background-size: 100% 2px; animation: cm-attr-sweep ${SWEEP_MS}ms ease-out forwards; animation-delay: -${Math.round(age)}ms;`;
      } else {
        // Sweep done — static full-width underline, fading via color alpha.
        underlineStyle = `${bg} background-size: 100% 2px;`;
      }
    }

    const tooltip = mark.actors.join(", ");

    builder.add(
      mark.from,
      mark.to,
      Decoration.mark({
        class: "cm-text-attribution",
        attributes: {
          style: `${opacityStyle} ${underlineStyle} transition: opacity ${TICK_MS}ms linear;`,
          title: tooltip,
        },
      }),
    );
  }

  return builder.finish();
}

// ── Decoration state field ───────────────────────────────────────────

const decorationsField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(decos, tr) {
    const needsRebuild =
      tr.docChanged || tr.effects.some((e) => e.is(addAttributionsEffect) || e.is(tickEffect));

    if (!needsRebuild) return decos;

    return buildDecorations(tr.state.field(marksField));
  },
  provide: (f) => EditorView.decorations.from(f),
});

// ── Tick / prune plugin ──────────────────────────────────────────────

/**
 * Runs a periodic timer that dispatches `tickEffect` to fade and prune
 * expired marks. The timer only runs while there are active marks.
 */
const attributionTickPlugin = ViewPlugin.fromClass(
  class {
    timer: ReturnType<typeof setInterval> | null = null;
    view: EditorView;

    constructor(view: EditorView) {
      this.view = view;
      this.maybeStartTimer();
    }

    update(update: ViewUpdate) {
      for (const effect of update.transactions.flatMap((t) => t.effects)) {
        if (effect.is(addAttributionsEffect)) {
          this.maybeStartTimer();
          return;
        }
      }
    }

    maybeStartTimer() {
      if (this.timer !== null) return;
      this.timer = setInterval(() => {
        const marks = this.view.state.field(marksField);
        if (marks.length === 0) {
          if (this.timer !== null) {
            clearInterval(this.timer);
            this.timer = null;
          }
          return;
        }
        this.view.dispatch({ effects: tickEffect.of(null) });
      }, TICK_MS);
    }

    destroy() {
      if (this.timer !== null) {
        clearInterval(this.timer);
        this.timer = null;
      }
    }
  },
);

// ── Theme ────────────────────────────────────────────────────────────

const attributionTheme = EditorView.theme({
  ".cm-text-attribution": {
    // Pad the bottom slightly so the background-image underline doesn't
    // overlap descenders too aggressively.
    paddingBottom: "1px",
  },
});

// ── Public API ───────────────────────────────────────────────────────

/**
 * CodeMirror extension for rendering text attribution highlights.
 *
 * Add to the editor's extensions array. Then call `addTextAttributions()`
 * to push highlight ranges from outside React.
 */
export function textAttributionExtension(): Extension[] {
  injectKeyframes();

  return [marksField, decorationsField, attributionTickPlugin, attributionTheme];
}

/**
 * Push new text attribution highlights into an EditorView.
 *
 * Each mark specifies a character range (`from`, `to`) and the actors
 * who authored it. The highlight will hold at full opacity for
 * `HOLD_MS` then fade out over `FADE_MS`.
 *
 * Safe to call at high frequency — marks are additive and independently timed.
 */
export function addTextAttributions(view: EditorView, marks: AttributionMark[]): void {
  if (marks.length === 0) return;
  view.dispatch({ effects: addAttributionsEffect.of(marks) });
}
