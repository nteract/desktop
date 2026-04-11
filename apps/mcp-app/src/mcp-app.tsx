import { createRoot } from "react-dom/client";
import { useEffect, useState } from "react";
import "./style.css";
import {
  App,
  applyDocumentTheme,
  applyHostStyleVariables,
  applyHostFonts,
  type McpUiHostContext,
} from "@modelcontextprotocol/ext-apps";
import type { CallToolResult } from "@modelcontextprotocol/sdk/types.js";
import type { NteractContent } from "./types";
import { Cell } from "./components/cell";
import { SummaryHeader } from "./components/summary-header";
import { hasRichOutput } from "./lib/rich-output";

/**
 * Collapse the widget to 0px when there's nothing to render.
 * Only collapse when there is truly no structured content — not when
 * cells exist but have empty outputs (those still show cell headers).
 */
function useCollapseWhenEmpty(hasCells: boolean) {
  useEffect(() => {
    const body = document.body;
    if (hasCells) {
      body.style.removeProperty("height");
      body.style.removeProperty("overflow");
    } else {
      body.style.height = "0px";
      body.style.overflow = "hidden";
    }
  }, [hasCells]);
}

function McpApp() {
  const [content, setContent] = useState<NteractContent | null>(null);
  const [allExpanded, setAllExpanded] = useState<boolean | null>(null);

  useEffect(() => {
    const app = new App({ name: "nteract", version: "0.1.0" });

    app.ontoolresult = (result: CallToolResult) => {
      const structured = result.structuredContent as NteractContent | undefined;
      if (!structured) return;
      setContent(structured);
      setAllExpanded(null); // Reset expand-all state for new content
    };

    app.onhostcontextchanged = (ctx: McpUiHostContext) => {
      if (ctx.theme) applyDocumentTheme(ctx.theme);
      if (ctx.styles?.variables) applyHostStyleVariables(ctx.styles.variables);
      if (ctx.styles?.css?.fonts) applyHostFonts(ctx.styles.css.fonts);
    };

    app.onerror = console.error;

    // Apply initial theme after connecting
    app.connect().then(() => {
      const ctx = app.getHostContext();
      if (ctx?.theme) applyDocumentTheme(ctx.theme);
      if (ctx?.styles?.variables) applyHostStyleVariables(ctx.styles.variables);
      if (ctx?.styles?.css?.fonts) applyHostFonts(ctx.styles.css.fonts);
    });

    return () => {
      setContent(null);
    };
  }, []);

  const cells = content?.cells || (content?.cell ? [content.cell] : []);
  const isMultiCell = cells.length > 1;

  useCollapseWhenEmpty(cells.length > 0);

  const blobBaseUrl = content?.blob_base_url;

  if (cells.length === 0) return null;

  return (
    <>
      {isMultiCell && (
        <SummaryHeader
          cells={cells}
          allExpanded={allExpanded ?? false}
          onToggleAll={() => setAllExpanded((prev) => !(prev ?? false))}
        />
      )}
      {cells.map((cell) => (
        <Cell
          key={cell.cell_id}
          cell={cell}
          blobBaseUrl={blobBaseUrl}
          defaultExpanded={!isMultiCell || hasRichOutput(cell)}
          forceExpanded={isMultiCell ? allExpanded : null}
          hideSource={!isMultiCell}
        />
      ))}
    </>
  );
}

const root = createRoot(document.getElementById("root")!);
root.render(<McpApp />);
