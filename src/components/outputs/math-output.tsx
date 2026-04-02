import katex from "katex";
import { useMemo } from "react";
import { cn } from "@/lib/utils";

import "katex/dist/katex.min.css";

interface MathOutputProps {
  /** Raw LaTeX string, possibly wrapped in $...$ or $$...$$ delimiters */
  content: string;
  className?: string;
}

/**
 * Strip $/$$ delimiters and detect display mode.
 * Sympy wraps in `$\displaystyle ...$`, other CAS may use `$$...$$`.
 * Raw LaTeX without delimiters defaults to display mode.
 */
function parseLatex(raw: string): { latex: string; displayMode: boolean } {
  const trimmed = raw.trim();
  if (trimmed.startsWith("$$") && trimmed.endsWith("$$")) {
    return { latex: trimmed.slice(2, -2).trim(), displayMode: true };
  }
  if (trimmed.startsWith("$") && trimmed.endsWith("$")) {
    return { latex: trimmed.slice(1, -1).trim(), displayMode: true };
  }
  // \begin{...} environments and bare LaTeX — display mode
  return { latex: trimmed, displayMode: true };
}

/**
 * Renders a `text/latex` MIME output using KaTeX.
 *
 * Used for display_data / execute_result from CAS kernels (sympy, Sage, etc.).
 * KaTeX output is safe static HTML — no iframe isolation needed.
 */
export function MathOutput({ content, className }: MathOutputProps) {
  const html = useMemo(() => {
    if (!content.trim()) return null;
    const { latex, displayMode } = parseLatex(content);
    try {
      return katex.renderToString(latex, {
        displayMode,
        throwOnError: false,
        trust: true,
      });
    } catch {
      return null;
    }
  }, [content]);

  if (!html) {
    return (
      <pre className={cn("whitespace-pre-wrap text-sm", className)}>
        {content}
      </pre>
    );
  }

  return (
    <div
      data-slot="math-output"
      className={cn("py-1", className)}
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}
