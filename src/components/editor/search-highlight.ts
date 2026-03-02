import { type Extension, RangeSetBuilder } from "@codemirror/state";
import {
  Decoration,
  type DecorationSet,
  EditorView,
  ViewPlugin,
  type ViewUpdate,
} from "@codemirror/view";

const searchMatchMark = Decoration.mark({ class: "cm-global-find-match" });
const activeMatchMark = Decoration.mark({
  class: "cm-global-find-match-active",
});

const searchHighlightTheme = EditorView.theme({
  ".cm-global-find-match": {
    background: "#fbbf24",
    color: "#000",
    borderRadius: "2px",
  },
  ".cm-global-find-match-active": {
    background: "#f97316",
    color: "#000",
    borderRadius: "2px",
  },
});

/**
 * Build decorations for all search matches in a document.
 * @param doc - The CodeMirror document text
 * @param query - Search query (case-insensitive)
 * @param activeOffset - Character offset of the active match (-1 for none)
 */
function buildDecorations(
  doc: string,
  query: string,
  activeOffset: number,
): DecorationSet {
  if (!query) return Decoration.none;

  const builder = new RangeSetBuilder<Decoration>();
  const lowerDoc = doc.toLowerCase();
  const lowerQuery = query.toLowerCase();
  let pos = lowerDoc.indexOf(lowerQuery, 0);

  while (pos !== -1) {
    const isActive = pos === activeOffset;
    builder.add(
      pos,
      pos + query.length,
      isActive ? activeMatchMark : searchMatchMark,
    );
    pos = lowerDoc.indexOf(lowerQuery, pos + query.length);
  }

  return builder.finish();
}

/**
 * Create a search highlight ViewPlugin for a given query.
 *
 * Note: Since ViewPlugin instances are static (cannot update config),
 * this returns a new extension each time the query changes. The parent
 * component should replace extensions when the query changes, which
 * is the standard pattern for react-codemirror.
 */
function createSearchHighlightPlugin(query: string, activeOffset: number) {
  return ViewPlugin.define(
    (view) => ({
      decorations: buildDecorations(
        view.state.doc.toString(),
        query,
        activeOffset,
      ),
      update(update: ViewUpdate) {
        if (update.docChanged) {
          this.decorations = buildDecorations(
            update.state.doc.toString(),
            query,
            activeOffset,
          );
        }
      },
    }),
    {
      decorations: (v) => v.decorations,
    },
  );
}

/**
 * Create a CodeMirror extension that highlights all matches of a search query.
 *
 * @param query - The search string to highlight (case-insensitive). Empty string = no highlights.
 * @param activeOffset - Character offset of the "active" match to highlight differently (-1 for none).
 * @returns A CodeMirror Extension array to pass to the editor.
 */
export function searchHighlight(query: string, activeOffset = -1): Extension[] {
  if (!query) return [];
  return [
    searchHighlightTheme,
    createSearchHighlightPlugin(query, activeOffset),
  ];
}
