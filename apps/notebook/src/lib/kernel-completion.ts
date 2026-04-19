import {
  autocompletion,
  type CompletionContext,
  type CompletionResult,
} from "@codemirror/autocomplete";
import type { Extension } from "@codemirror/state";
import type { NotebookHost } from "@nteract/notebook-host";
import { NotebookClient } from "runtimed";

/** Module-level reference to the host — set once from `main.tsx`. */
let _host: NotebookHost | null = null;
let _client: NotebookClient | null = null;

/**
 * Register the `NotebookHost` used for kernel completions. Called once
 * from `main.tsx` after `createTauriHost()`. Matches the pattern used by
 * `logger`, `blob-port`, and `open-url` for non-React helpers.
 */
export function setKernelCompletionHost(host: NotebookHost | null): void {
  _host = host;
  _client = host ? new NotebookClient({ transport: host.transport }) : null;
}

/**
 * CodeMirror completion source that queries the Jupyter kernel for code
 * completions via the `complete` request.
 *
 * Only activates on explicit request (Ctrl+Space / Tab) to avoid per-
 * keystroke kernel round-trips that thrash busy→idle status and
 * generate excessive Automerge sync traffic.
 */
async function kernelCompletionSource(
  context: CompletionContext,
): Promise<CompletionResult | null> {
  if (!context.explicit) return null;
  if (!_client || !_host) return null;

  const code = context.state.doc.toString();
  const cursorPos = context.pos;

  try {
    const result = await _client.complete(code, cursorPos);

    if (context.aborted) return null;
    if (!result.items || result.items.length === 0) return null;

    return {
      from: result.cursorStart,
      to: result.cursorEnd,
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
  activateOnTyping: false,
});
