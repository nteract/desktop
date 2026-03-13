/**
 * CodeMirror 6 extension for rendering remote peer cursors and selections.
 *
 * Architecture:
 * - `StateEffect<RemoteCursorState[]>` — dispatched from outside to update positions
 * - `StateField<RemoteCursorState[]>` — stores current cursor state per peer
 * - `layer()` — renders cursor markers as absolutely-positioned elements,
 *   bypassing CM's content DOM reconciliation entirely (no ghost nodes)
 * - `StateField<DecorationSet>` — selection highlights via Decoration.mark()
 *
 * Cursors use CM6's `layer` API — the same mechanism behind `drawSelection`.
 * Layer markers live in a container outside the content DOM, so CM6 manages
 * their lifecycle via explicit draw/update/eq rather than decoration-set
 * diffing. This eliminates the ghost widget accumulation that occurs with
 * Decoration.widget under rapid position changes.
 *
 * Selections use Decoration.mark() range decorations which don't have the
 * ghost DOM issue (they paint CSS backgrounds on existing content nodes).
 *
 * The hot path (cursor position updates) is purely imperative — no React.
 * Call `setRemoteCursors(view, cursors)` to push new positions into an EditorView.
 */

import {
  type Extension,
  RangeSetBuilder,
  StateEffect,
  StateField,
} from "@codemirror/state";
import {
  Decoration,
  type DecorationSet,
  EditorView,
  type LayerMarker,
  layer,
} from "@codemirror/view";

// ── Types ────────────────────────────────────────────────────────────

export interface RemoteCursorState {
  peerId: string;
  peerLabel: string;
  /** 0-based line number */
  line: number;
  /** 0-based column */
  column: number;
  color: string;
}

export interface RemoteSelectionState {
  peerId: string;
  peerLabel: string;
  anchorLine: number;
  anchorCol: number;
  headLine: number;
  headCol: number;
  color: string;
}

// ── Color palette ────────────────────────────────────────────────────

const CURSOR_COLORS = [
  "#2563eb", // blue
  "#e11d48", // rose
  "#d97706", // amber
  "#059669", // emerald
  "#7c3aed", // violet
  "#0891b2", // cyan
  "#db2777", // pink
  "#65a30d", // lime
];

/** Deterministic color from peer ID. */
export function peerColor(peerId: string): string {
  let hash = 0;
  for (let i = 0; i < peerId.length; i++) {
    hash = (hash * 31 + peerId.charCodeAt(i)) | 0;
  }
  return CURSOR_COLORS[Math.abs(hash) % CURSOR_COLORS.length];
}

// ── State effects ────────────────────────────────────────────────────

const setCursorsEffect = StateEffect.define<RemoteCursorState[]>();
const setSelectionsEffect = StateEffect.define<RemoteSelectionState[]>();

// ── State fields (raw data) ──────────────────────────────────────────

const cursorsField = StateField.define<RemoteCursorState[]>({
  create: () => [],
  update(value, tr) {
    for (const e of tr.effects) {
      if (e.is(setCursorsEffect)) return e.value;
    }
    return value;
  },
});

const selectionsField = StateField.define<RemoteSelectionState[]>({
  create: () => [],
  update(value, tr) {
    for (const e of tr.effects) {
      if (e.is(setSelectionsEffect)) return e.value;
    }
    return value;
  },
});

// ── Position resolver ────────────────────────────────────────────────

/** Convert 0-based line:column to a document position, clamped to bounds. */
function resolvePos(
  doc: { lines: number; line: (n: number) => { from: number; length: number } },
  cursor: { line: number; column: number },
): number {
  const lineCount = doc.lines;
  const safeLine = Math.max(0, cursor.line);
  const lineNum = Math.min(safeLine + 1, lineCount); // CM uses 1-based lines
  const line = doc.line(lineNum);
  const col = Math.min(Math.max(0, cursor.column), line.length);
  return line.from + col;
}

// ── Cursor layer marker ──────────────────────────────────────────────

/**
 * A single remote cursor rendered as an absolutely-positioned element
 * inside CM6's layer container. Implements LayerMarker so CM6 manages
 * DOM lifecycle directly — no decoration diffing.
 */
class CursorMarker implements LayerMarker {
  constructor(
    readonly left: number,
    readonly top: number,
    readonly height: number,
    readonly color: string,
    readonly label: string,
    readonly peerId: string,
  ) {}

  eq(other: LayerMarker): boolean {
    if (!(other instanceof CursorMarker)) return false;
    return (
      this.left === other.left &&
      this.top === other.top &&
      this.height === other.height &&
      this.color === other.color &&
      this.label === other.label &&
      this.peerId === other.peerId
    );
  }

  draw(): HTMLElement {
    const el = document.createElement("div");
    el.className = "cm-remote-cursor";
    el.style.left = `${this.left}px`;
    el.style.top = `${this.top}px`;
    el.style.height = `${this.height}px`;
    el.setAttribute("aria-label", this.label || "Remote cursor");

    const bar = document.createElement("div");
    bar.className = "cm-remote-cursor-bar";
    bar.style.backgroundColor = this.color;
    el.appendChild(bar);

    if (this.label) {
      const flag = document.createElement("div");
      flag.className = "cm-remote-cursor-flag";
      flag.style.backgroundColor = this.color;
      flag.textContent = this.label;
      el.appendChild(flag);
    }

    return el;
  }

  update(dom: HTMLElement, prev: LayerMarker): boolean {
    if (!(prev instanceof CursorMarker)) return false;
    // Color or label changed → recreate DOM
    if (this.color !== prev.color || this.label !== prev.label) return false;
    // Reposition in-place (fast path for cursor movement)
    dom.style.left = `${this.left}px`;
    dom.style.top = `${this.top}px`;
    dom.style.height = `${this.height}px`;
    return true;
  }
}

// ── Cursor layer ─────────────────────────────────────────────────────
//
// Uses CM6's layer API — the same mechanism behind drawSelection.
// Markers are absolutely positioned in a container that scrolls with
// the editor content. CM6 calls markers() to measure positions, then
// diffs by array index using eq()/update()/draw().

const remoteCursorLayer = layer({
  above: true,
  class: "cm-remote-cursors-layer",

  markers(view) {
    const cursors = view.state.field(cursorsField);
    if (cursors.length === 0) return [];

    const markers: CursorMarker[] = [];
    const scrollRect = view.scrollDOM.getBoundingClientRect();

    for (const cursor of cursors) {
      const pos = resolvePos(view.state.doc, cursor);
      const coords = view.coordsAtPos(pos, 1);
      if (!coords) continue;

      // Convert viewport coords → content-space coords
      // (layer container is absolutely positioned inside scrollDOM)
      const left = coords.left - scrollRect.left + view.scrollDOM.scrollLeft;
      const top = coords.top - scrollRect.top + view.scrollDOM.scrollTop;
      const height = coords.bottom - coords.top;

      markers.push(
        new CursorMarker(
          left,
          top,
          height,
          cursor.color,
          cursor.peerLabel,
          cursor.peerId,
        ),
      );
    }

    // Stable ordering by peerId so CM6 matches old↔new markers by index,
    // enabling in-place repositioning via update() instead of full recreate.
    markers.sort((a, b) =>
      a.peerId < b.peerId ? -1 : a.peerId > b.peerId ? 1 : 0,
    );

    return markers;
  },

  update(update, _layerElement) {
    // Remeasure when cursor state changes
    for (const tr of update.transactions) {
      for (const e of tr.effects) {
        if (e.is(setCursorsEffect)) return true;
      }
    }
    // Doc changes shift positions; geometry handled by updateOnDocViewUpdate (default: true)
    return update.docChanged;
  },
});

// ── Selection decorations ────────────────────────────────────────────

function buildSelectionDecorations(
  doc: { lines: number; line: (n: number) => { from: number; length: number } },
  selections: RemoteSelectionState[],
): DecorationSet {
  if (selections.length === 0) return Decoration.none;

  const builder = new RangeSetBuilder<Decoration>();
  const lineCount = doc.lines;

  // Collect and sort ranges
  const ranges: { from: number; to: number; color: string }[] = [];

  for (const sel of selections) {
    const anchorLineNum = Math.max(1, Math.min(sel.anchorLine + 1, lineCount));
    const anchorLine = doc.line(anchorLineNum);
    const anchorCol = Math.min(Math.max(0, sel.anchorCol), anchorLine.length);
    const anchorPos = anchorLine.from + anchorCol;

    const headLineNum = Math.max(1, Math.min(sel.headLine + 1, lineCount));
    const headLine = doc.line(headLineNum);
    const headCol = Math.min(Math.max(0, sel.headCol), headLine.length);
    const headPos = headLine.from + headCol;

    const from = Math.min(anchorPos, headPos);
    const to = Math.max(anchorPos, headPos);

    if (from < to) {
      ranges.push({ from, to, color: sel.color });
    }
  }

  // RangeSetBuilder requires sorted, non-overlapping additions
  ranges.sort((a, b) => a.from - b.from || a.to - b.to);

  for (const { from, to, color } of ranges) {
    builder.add(
      from,
      to,
      Decoration.mark({
        class: "cm-remote-selection",
        attributes: {
          style: `background-color: ${color}33`, // ~20% opacity via hex alpha
        },
      }),
    );
  }

  return builder.finish();
}

const selectionDecorationsField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(decos, tr) {
    for (const e of tr.effects) {
      if (e.is(setSelectionsEffect)) {
        return buildSelectionDecorations(tr.state.doc, e.value);
      }
    }
    if (tr.docChanged) return decos.map(tr.changes);
    return decos;
  },
  provide: (f) => EditorView.decorations.from(f),
});

// ── Theme ────────────────────────────────────────────────────────────

const remoteCursorsTheme = EditorView.theme({
  ".cm-remote-cursors-layer": {
    pointerEvents: "none",
  },
  ".cm-remote-cursor": {
    position: "absolute",
    pointerEvents: "none",
  },
  ".cm-remote-cursor-bar": {
    position: "absolute",
    top: "0",
    bottom: "0",
    left: "-1px",
    width: "2.5px",
    borderRadius: "1px",
  },
  ".cm-remote-cursor-flag": {
    position: "absolute",
    bottom: "100%",
    left: "-1px",
    marginBottom: "1px",
    padding: "2px 6px",
    borderRadius: "4px 4px 4px 0",
    fontSize: "11px",
    lineHeight: "1.2",
    fontWeight: "500",
    fontFamily: "system-ui, -apple-system, sans-serif",
    color: "white",
    whiteSpace: "nowrap",
    pointerEvents: "none",
  },
  ".cm-remote-selection": {
    // Background color is set inline via decoration attributes
  },
});

// ── Public API ───────────────────────────────────────────────────────

/**
 * CodeMirror extension for rendering remote cursors and selections.
 *
 * Add to the editor's extensions array. Then call `setRemoteCursors()` and
 * `setRemoteSelections()` to update positions from outside React.
 */
export function remoteCursorsExtension(): Extension[] {
  return [
    cursorsField,
    selectionsField,
    remoteCursorLayer,
    selectionDecorationsField,
    remoteCursorsTheme,
  ];
}

/**
 * Push new remote cursor positions into an EditorView.
 *
 * Dispatches a StateEffect that triggers the cursor layer to remeasure
 * marker positions. Safe to call at high frequency (50+ updates/sec).
 */
export function setRemoteCursors(
  view: EditorView,
  cursors: RemoteCursorState[],
): void {
  view.dispatch({ effects: setCursorsEffect.of(cursors) });
}

/**
 * Push new remote selection ranges into an EditorView.
 */
export function setRemoteSelections(
  view: EditorView,
  selections: RemoteSelectionState[],
): void {
  view.dispatch({ effects: setSelectionsEffect.of(selections) });
}
