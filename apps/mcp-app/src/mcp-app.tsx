import { createRoot } from "react-dom/client";
import { useEffect, useRef, useState } from "react";
import {
  App,
  applyDocumentTheme,
  type McpUiHostContext,
} from "@modelcontextprotocol/ext-apps";
import type { CallToolResult } from "@modelcontextprotocol/sdk/types.js";
import type { NteractContent } from "./types";
import { CellOutput } from "./components/cell-output";

/**
 * Collapse the widget to 0px when there's nothing to render.
 * This prevents empty MCP App containers from showing for tools
 * like replace_match that don't produce cell outputs.
 */
function useCollapseWhenEmpty(ref: React.RefObject<HTMLDivElement | null>, hasContent: boolean) {
  useEffect(() => {
    const body = document.body;
    if (hasContent) {
      body.style.removeProperty("height");
      body.style.removeProperty("overflow");
    } else {
      body.style.height = "0px";
      body.style.overflow = "hidden";
    }
  }, [hasContent]);
}

function McpApp() {
  const [content, setContent] = useState<NteractContent | null>(null);
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const app = new App({ name: "nteract", version: "0.1.0" });

    app.ontoolresult = (result: CallToolResult) => {
      const structured = result.structuredContent as NteractContent | undefined;
      if (!structured) return;
      setContent(structured);
    };

    app.onhostcontextchanged = (ctx: McpUiHostContext) => {
      if (ctx.theme) applyDocumentTheme(ctx.theme);
    };

    app.onerror = console.error;

    app.connect();

    return () => {
      setContent(null);
    };
  }, []);

  const cells = content?.cells || (content?.cell ? [content.cell] : []);
  const hasOutputs = cells.some((c) => c.outputs?.length > 0);

  useCollapseWhenEmpty(containerRef, hasOutputs);

  if (!hasOutputs) return null;

  return (
    <div ref={containerRef}>
      {cells.map((cell) => (
        <CellOutput key={cell.cell_id} cell={cell} />
      ))}
    </div>
  );
}

const root = createRoot(document.getElementById("root")!);
root.render(<McpApp />);
