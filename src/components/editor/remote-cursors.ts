/**
 * CodeMirror 6 extension for rendering remote peer cursors and selections.
 *
 * Architecture:
 * - `StateEffect<RemoteCursorState[]>` — dispatched from outside to update positions
 * - `StateField<RemoteCursorState[]>` — stores current cursors for this editor instance
 * - `ViewPlugin` — reads the StateField and produces Decoration widgets/marks
 * - `WidgetType` subclass — renders the colored cursor bar with peer name tooltip
 *
 * The hot path (cursor position updates) is purely imperative — no React involvement.
 * Call `setRemoteCursors(view, cursors)` to push new positions into an EditorView.
 */

import {
  type Extension,
  type Range,
  RangeSetBuilder,
  StateEffect,
  StateField,
} from "@codemirror/state";
import {
  Decoration,
  type DecorationSet,
  EditorView,
  ViewPlugin,
  type ViewUpdate,
  WidgetType,
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

// ── State fields ─────────────────────────────────────────────────────

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

// ── Cursor widget ────────────────────────────────────────────────────

class CursorWidget extends WidgetType {
  constructor(
    readonly color: string,
    readonly label: string,
  ) {
    super();
  }

  eq(other: CursorWidget): boolean {
    return this.color === other.color && this.label === other.label;
  }

  toDOM(): HTMLElement {
    const wrapper = document.createElement("span");
    wrapper.className = "cm-remote-cursor";
    wrapper.style.borderLeftColor = this.color;
    wrapper.setAttribute("aria-label", this.label || "Remote cursor");

    // Name label (shown on hover via CSS)
    if (this.label) {
      const tag = document.createElement("span");
      tag.className = "cm-remote-cursor-label";
      tag.style.backgroundColor = this.color;
      tag.textContent = this.label;
      wrapper.appendChild(tag);
    }

    return wrapper;
  }

  ignoreEvent(): boolean {
    return true;
  }
}

// ── Decoration builder ───────────────────────────────────────────────

function buildCursorDecorations(
  view: EditorView,
  cursors: RemoteCursorState[],
): DecorationSet {
  if (cursors.length === 0) return Decoration.none;

  const doc = view.state.doc;
  const widgets: Range<Decoration>[] = [];

  for (const cursor of cursors) {
    // Clamp to document bounds
    const lineCount = doc.lines;
    const lineNum = Math.min(cursor.line + 1, lineCount); // CM lines are 1-based
    const line = doc.line(lineNum);
    const col = Math.min(cursor.column, line.length);
    const pos = line.from + col;

    widgets.push(
      Decoration.widget({
        widget: new CursorWidget(cursor.color, cursor.peerLabel),
        side: 1, // render after the character
      }).range(pos),
    );
  }

  // Decorations must be sorted by position
  widgets.sort((a, b) => a.from - b.from);
  return Decoration.set(widgets);
}

function buildSelectionDecorations(
  view: EditorView,
  selections: RemoteSelectionState[],
): DecorationSet {
  if (selections.length === 0) return Decoration.none;

  const doc = view.state.doc;
  const builder = new RangeSetBuilder<Decoration>();
  const lineCount = doc.lines;

  // Collect and sort ranges
  const ranges: { from: number; to: number; color: string }[] = [];

  for (const sel of selections) {
    const anchorLineNum = Math.min(sel.anchorLine + 1, lineCount);
    const anchorLine = doc.line(anchorLineNum);
    const anchorCol = Math.min(sel.anchorCol, anchorLine.length);
    const anchorPos = anchorLine.from + anchorCol;

    const headLineNum = Math.min(sel.headLine + 1, lineCount);
    const headLine = doc.line(headLineNum);
    const headCol = Math.min(sel.headCol, headLine.length);
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

// ── View plugin ──────────────────────────────────────────────────────

class RemoteCursorsPlugin {
  cursorDecorations: DecorationSet;
  selectionDecorations: DecorationSet;

  constructor(view: EditorView) {
    this.cursorDecorations = buildCursorDecorations(
      view,
      view.state.field(cursorsField),
    );
    this.selectionDecorations = buildSelectionDecorations(
      view,
      view.state.field(selectionsField),
    );
  }

  update(update: ViewUpdate) {
    // Rebuild decorations when cursor/selection state changes or document changes
    // (document changes invalidate positions)
    let cursorsChanged = update.docChanged;
    let selectionsChanged = update.docChanged;

    for (const e of update.transactions) {
      for (const eff of e.effects) {
        if (eff.is(setCursorsEffect)) cursorsChanged = true;
        if (eff.is(setSelectionsEffect)) selectionsChanged = true;
      }
    }

    if (cursorsChanged) {
      this.cursorDecorations = buildCursorDecorations(
        update.view,
        update.state.field(cursorsField),
      );
    }
    if (selectionsChanged) {
      this.selectionDecorations = buildSelectionDecorations(
        update.view,
        update.state.field(selectionsField),
      );
    }
  }
}

const cursorPlugin = ViewPlugin.fromClass(RemoteCursorsPlugin, {
  decorations: (v) => v.cursorDecorations,
});

const selectionPlugin = ViewPlugin.fromClass(
  // Reuse the same class but expose selection decorations
  // We need a separate plugin instance because CM6 only allows one
  // decoration source per plugin. Use a thin wrapper.
  class {
    source: RemoteCursorsPlugin | null = null;

    constructor(view: EditorView) {
      // Access the sibling plugin — they share the same StateField
      this.source = view.plugin(cursorPlugin);
    }

    get decorations(): DecorationSet {
      return this.source?.selectionDecorations ?? Decoration.none;
    }

    update(update: ViewUpdate) {
      // The source plugin handles rebuild; just re-read the reference
      this.source = update.view.plugin(cursorPlugin);
    }
  },
  {
    decorations: (v) => v.decorations,
  },
);

// ── Theme ────────────────────────────────────────────────────────────

const remoteCursorsTheme = EditorView.theme({
  ".cm-remote-cursor": {
    position: "relative",
    borderLeft: "2px solid",
    marginLeft: "-1px",
    marginRight: "-1px",
    pointerEvents: "none",
  },
  ".cm-remote-cursor-label": {
    position: "absolute",
    bottom: "100%",
    left: "-1px",
    padding: "1px 4px",
    borderRadius: "3px 3px 3px 0",
    fontSize: "11px",
    lineHeight: "14px",
    fontFamily: "system-ui, sans-serif",
    color: "white",
    whiteSpace: "nowrap",
    pointerEvents: "none",
    opacity: "0",
    transition: "opacity 0.15s ease",
    zIndex: "10",
  },
  ".cm-remote-cursor:hover .cm-remote-cursor-label": {
    opacity: "1",
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
    cursorPlugin,
    selectionPlugin,
    remoteCursorsTheme,
  ];
}

/**
 * Push new remote cursor positions into an EditorView.
 *
 * This dispatches a StateEffect — the ViewPlugin will pick up the change
 * and rebuild decorations. Safe to call at high frequency.
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
