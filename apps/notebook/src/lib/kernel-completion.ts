import {
  autocompletion,
  type CompletionContext,
  type CompletionResult,
} from "@codemirror/autocomplete";
import type { Extension } from "@codemirror/state";
import { invoke } from "@tauri-apps/api/core";

/** A single completion item (LSP-ready structure). */
interface CompletionItem {
  label: string;
  /** Kind: "function", "variable", "class", "module", etc. */
  kind?: string;
  /** Short type annotation. */
  detail?: string;
  /** Source: "kernel" now, "ruff"/"basedpyright" later. */
  source?: string;
}

interface KernelCompletionResult {
  items: CompletionItem[];
  cursor_start: number;
  cursor_end: number;
}

/**
 * CodeMirror completion source that queries the Jupyter kernel
 * for code completions via the `complete_request` message.
 */
async function kernelCompletionSource(
  context: CompletionContext,
): Promise<CompletionResult | null> {
  // Only trigger on explicit (Ctrl+Space) or after a dot/word character
  const word = context.matchBefore(/[\w.]+/);
  if (!word && !context.explicit) return null;

  const code = context.state.doc.toString();
  const cursorPos = context.pos;

  try {
    const result = await invoke<KernelCompletionResult>("complete_via_daemon", {
      code,
      cursorPos,
    });

    // Discard stale response if the user has moved on
    if (context.aborted) return null;
    if (!result.items || result.items.length === 0) return null;

    return {
      from: result.cursor_start,
      to: result.cursor_end,
      options: result.items.map((item) => ({ label: item.label })),
    };
  } catch {
    // Kernel not running or request failed — silently return no completions
    return null;
  }
}

/**
 * CodeMirror extension that provides Jupyter kernel-based tab completion.
 * Add this to the editor's extensions to enable it.
 */
export const kernelCompletionExtension: Extension = autocompletion({
  override: [kernelCompletionSource],
  activateOnTypingDelay: 150,
});
