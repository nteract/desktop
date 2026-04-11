import { indentService } from "@codemirror/language";
import { RangeSetBuilder } from "@codemirror/state";
import type { DecorationSet, ViewUpdate } from "@codemirror/view";
import { Decoration, EditorView, ViewPlugin } from "@codemirror/view";

/**
 * IPython syntax highlighting extension for CodeMirror 6
 *
 * Highlights IPython-specific syntax on top of standard Python:
 * - Shell commands: !ls, !pip install
 * - Line magics: %time, %run script.py
 * - Cell magics: %%bash, %%javascript (first line only)
 * - Help operators: object?, object??
 *
 * Note: For cell magics (%%), only the first line is decorated.
 * The rest of the cell should use the appropriate language mode.
 * Use detectCellMagic() and CELL_MAGIC_LANGUAGES to determine
 * the correct language for cell magic content.
 */

// Decoration marks for IPython syntax
const shellMark = Decoration.mark({ class: "cm-ipython-shell" });
const magicMark = Decoration.mark({ class: "cm-ipython-magic" });
const cellMagicMark = Decoration.mark({ class: "cm-ipython-cell-magic" });
const helpMark = Decoration.mark({ class: "cm-ipython-help" });

// Patterns for IPython syntax (applied to line content)
const CELL_MAGIC_PATTERN = /^(%%[a-zA-Z_]\w*)/;
const LINE_MAGIC_PATTERN = /^(%[a-zA-Z_]\w*)/;
const SHELL_PATTERN = /^(!)/;
const HELP_PATTERN = /(\?\??)$/;

/**
 * Custom indentation service for IPython.
 *
 * Prevents auto-indent after:
 * - Line magics (%time, %matplotlib, etc.)
 * - Shell commands (!pip, !ls, etc.)
 * - Cell magic declarations (%%bash, %%html, etc.)
 *
 * These lines don't follow Python's indentation rules,
 * so we reset to column 0 after them.
 */
export const ipythonIndent = indentService.of((context, pos) => {
  // Get the previous line
  const line = context.state.doc.lineAt(pos);
  if (line.number <= 1) return null; // Let Python handle first line

  const prevLine = context.state.doc.line(line.number - 1);
  const prevText = prevLine.text.trim();

  // After line magic, shell command, or cell magic - don't auto-indent
  if (
    LINE_MAGIC_PATTERN.test(prevText) ||
    SHELL_PATTERN.test(prevText) ||
    CELL_MAGIC_PATTERN.test(prevText)
  ) {
    return 0;
  }

  // Let Python's indentation handle everything else
  return null;
});

/**
 * Mapping of cell magic names to language identifiers.
 * Use with getLanguageExtension() from languages.ts
 */
export const CELL_MAGIC_LANGUAGES: Record<string, string> = {
  // HTML/SVG
  html: "html",
  HTML: "html",
  svg: "html",
  SVG: "html",
  // JavaScript
  javascript: "javascript",
  js: "javascript",
  // TypeScript
  typescript: "typescript",
  ts: "typescript",
  // SQL
  sql: "sql",
  SQL: "sql",
  // Markdown
  markdown: "markdown",
  md: "markdown",
  // JSON
  json: "json",
  // Shell (falls back to plain since we don't have shell lang)
  bash: "plain",
  sh: "plain",
  // Python (stays as Python)
  python: "python",
  python3: "python",
  // Others without specific support fall back to plain
};

/**
 * Detect cell magic from content.
 * Cell magics must be on the first line.
 *
 * @param content - The editor content
 * @returns The magic name (without %%) if found, null otherwise
 *
 * @example
 * detectCellMagic("%%html\n<div>Hello</div>") // "html"
 * detectCellMagic("%time x = 1") // null (line magic, not cell magic)
 * detectCellMagic("print('hello')") // null
 */
export function detectCellMagic(content: string): string | null {
  const firstLine = content.split("\n")[0].trim();
  const match = firstLine.match(/^%%([a-zA-Z_]\w*)/);
  return match ? match[1] : null;
}

/**
 * Get the language identifier for a cell magic.
 *
 * @param magic - The cell magic name (without %%)
 * @returns The language identifier to use with getLanguageExtension(),
 *          or "plain" for unsupported magics
 *
 * @example
 * getCellMagicLanguage("html") // "html"
 * getCellMagicLanguage("bash") // "plain" (no shell support)
 * getCellMagicLanguage("unknown") // "plain"
 */
export function getCellMagicLanguage(magic: string): string {
  return CELL_MAGIC_LANGUAGES[magic] ?? "plain";
}

class IPythonHighlighter {
  decorations: DecorationSet;

  constructor(view: EditorView) {
    this.decorations = this.buildDecorations(view);
  }

  update(update: ViewUpdate) {
    if (update.docChanged || update.viewportChanged) {
      this.decorations = this.buildDecorations(update.view);
    }
  }

  buildDecorations(view: EditorView): DecorationSet {
    const builder = new RangeSetBuilder<Decoration>();
    const doc = view.state.doc;

    // Check if we're in cell magic mode (first line is %%)
    const firstLineText = doc.line(1).text.trim();
    const isInCellMagicMode = CELL_MAGIC_PATTERN.test(firstLineText);

    for (const { from, to } of view.visibleRanges) {
      const startLine = doc.lineAt(from).number;
      const endLine = doc.lineAt(to).number;

      for (let lineNum = startLine; lineNum <= endLine; lineNum++) {
        const line = doc.line(lineNum);
        const lineText = line.text;
        const trimmedText = lineText.trimStart();
        const leadingSpaces = lineText.length - trimmedText.length;
        const lineStart = line.from + leadingSpaces;

        // Cell magic on first line - decorate it
        if (lineNum === 1) {
          const cellMagicMatch = trimmedText.match(CELL_MAGIC_PATTERN);
          if (cellMagicMatch) {
            builder.add(lineStart, lineStart + cellMagicMatch[1].length, cellMagicMark);
            continue;
          }
        }

        // If we're in cell magic mode, don't apply IPython decorations
        // to subsequent lines (they belong to the cell magic's language)
        if (isInCellMagicMode && lineNum > 1) {
          continue;
        }

        // Line magic (%magic)
        const lineMagicMatch = trimmedText.match(LINE_MAGIC_PATTERN);
        if (lineMagicMatch) {
          builder.add(lineStart, lineStart + lineMagicMatch[1].length, magicMark);
          continue;
        }

        // Shell command (!command)
        const shellMatch = trimmedText.match(SHELL_PATTERN);
        if (shellMatch) {
          builder.add(lineStart, line.to, shellMark);
          continue;
        }

        // Help operator (object? or object??)
        const helpMatch = lineText.match(HELP_PATTERN);
        if (helpMatch && helpMatch.index !== undefined) {
          const helpStart = line.from + helpMatch.index;
          const helpEnd = helpStart + helpMatch[1].length;
          builder.add(helpStart, helpEnd, helpMark);
        }
      }
    }

    return builder.finish();
  }
}

/**
 * CodeMirror extension that adds IPython syntax highlighting.
 *
 * For cells with cell magics (%%html, %%bash, etc.), only the first
 * line is decorated. Use detectCellMagic() and getLanguageExtension()
 * to set the appropriate language for the cell content.
 */
export function ipythonHighlighting() {
  return ViewPlugin.fromClass(IPythonHighlighter, {
    decorations: (v) => v.decorations,
  });
}

/**
 * Light theme styles for IPython syntax
 */
export const ipythonStyles = EditorView.theme({
  ".cm-ipython-shell": {
    color: "#0550ae",
  },
  ".cm-ipython-magic": {
    color: "#6639ba",
  },
  ".cm-ipython-cell-magic": {
    color: "#6639ba",
    fontWeight: "bold",
  },
  ".cm-ipython-help": {
    color: "#0969da",
  },
});

/**
 * Dark theme styles for IPython syntax
 */
export const ipythonStylesDark = EditorView.theme(
  {
    ".cm-ipython-shell": {
      color: "#79c0ff",
    },
    ".cm-ipython-magic": {
      color: "#d2a8ff",
    },
    ".cm-ipython-cell-magic": {
      color: "#d2a8ff",
      fontWeight: "bold",
    },
    ".cm-ipython-help": {
      color: "#58a6ff",
    },
  },
  { dark: true },
);
