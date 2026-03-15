/**
 * CodeMirror 6 extension for rendering text attribution highlights.
 *
 * When remote peers (agents, other humans) edit cell source text, the WASM
 * layer pushes `TextAttribution` ranges via the frame bus. This extension
 * renders those ranges as translucent background highlights that fade out
 * over a configurable duration, giving visual feedback about who wrote what.
 *
 * Architecture:
 * - `StateEffect<AttributionMark[]>` — dispatched from outside to add highlights
 * - `StateField<AttributionMark[]>` — stores active marks with timestamps
 * - `DecorationSet` — built from active marks, rebuilt on tick/prune
 * - `ViewPlugin` — runs a periodic prune loop to fade and remove expired marks
 *
 * The hot path is purely imperative — no React. Call
 * `addTextAttributions(view, marks)` to push new highlights into an EditorView.
 *
 * Colors default to a translucent teal but can be matched to presence cursor
 * colors by passing a `color` field derived from `peerColor(actorId)`.
 */

import {
  RangeSetBuilder,
  StateEffect,
  StateField,
  type Extension,
  type Transaction,
} from "@codemirror/state";
import {
  Decoration,
  type DecorationSet,
  EditorView,
  ViewPlugin,
  type ViewUpdate,
} from "@codemirror/view";

// ── Configuration ────────────────────────────────────────────────────

/** How long (ms) a highlight remains fully visible before starting to fade. */
const HOLD_MS = 1500;

/** How long (ms) the fade-out transition takes after the hold period. */
const FADE_MS = 2000;

/** Total lifetime of a highlight (hold + fade). */
const TOTAL_MS = HOLD_MS + FADE_MS;

/** How often (ms) the prune loop runs to update opacity and remove expired marks. */
const TICK_MS = 100;

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
  if (Number.isNaN(r) || Number.isNaN(g) || Number.isNaN(b))
    return DEFAULT_COLOR;
  return `${r}, ${g}, ${b}`;
}

// ── State effect ─────────────────────────────────────────────────────

const addAttributionsEffect = StateEffect.define<AttributionMark[]>();

/**
 * Trigger a decoration rebuild on the next tick (prune/fade cycle).
 * The value is unused — its presence is the signal.
 */
const tickEffect = StateEffect.define<null>();

// ── State field ──────────────────────────────────────────────────────

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

    // If the document changed, remap positions through the change set.
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

    // Add new marks from effects
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

      // On tick, prune expired marks
      if (effect.is(tickEffect)) {
        const now = performance.now();
        updated = updated.filter((m) => now - m.createdAt < TOTAL_MS);
      }
    }

    return updated;
  },
});

// ── Decoration builder ───────────────────────────────────────────────

function buildDecorations(marks: TimedMark[]): DecorationSet {
  if (marks.length === 0) return Decoration.none;

  const now = performance.now();
  const builder = new RangeSetBuilder<Decoration>();

  // Sort by `from` position (RangeSetBuilder requires sorted input)
  const sorted = [...marks].sort((a, b) => a.from - b.from || a.to - b.to);

  for (const mark of sorted) {
    const age = now - mark.createdAt;
    if (age >= TOTAL_MS) continue;

    // Compute opacity: full during hold, linear fade after
    let opacity: number;
    if (age < HOLD_MS) {
      opacity = 0.3;
    } else {
      const fadeProgress = (age - HOLD_MS) / FADE_MS;
      opacity = 0.3 * (1 - fadeProgress);
    }

    if (opacity <= 0.005) continue;

    const rgbaColor = `rgba(${mark.color}, ${opacity.toFixed(3)})`;
    const tooltip = mark.actors.join(", ");

    builder.add(
      mark.from,
      mark.to,
      Decoration.mark({
        class: "cm-text-attribution",
        attributes: {
          style: `background-color: ${rgbaColor}; transition: background-color ${TICK_MS}ms linear;`,
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
    // Rebuild decorations when marks change (new attributions or tick)
    for (const effect of tr.effects) {
      if (effect.is(addAttributionsEffect) || effect.is(tickEffect)) {
        return buildDecorations(tr.state.field(marksField));
      }
    }
    // Remap decorations through document changes
    if (tr.docChanged) {
      return buildDecorations(tr.state.field(marksField));
    }
    return decos;
  },
  provide: (f) => EditorView.decorations.from(f),
});

// ── View plugin (tick/prune loop) ────────────────────────────────────

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
      // Start or stop the timer based on whether we have active marks
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
          // No active marks — stop ticking
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
    borderRadius: "2px",
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
  return [
    marksField,
    decorationsField,
    attributionTickPlugin,
    attributionTheme,
  ];
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
export function addTextAttributions(
  view: EditorView,
  marks: AttributionMark[],
): void {
  if (marks.length === 0) return;
  view.dispatch({ effects: addAttributionsEffect.of(marks) });
}
