import { createRoot } from "react-dom/client";
import { useState, useEffect } from "react";
import {
  App,
  applyDocumentTheme,
  type McpUiHostContext,
} from "@modelcontextprotocol/ext-apps";
import type { CallToolResult } from "@modelcontextprotocol/sdk/types.js";
import type { NteractContent } from "./types";
import { CellOutput } from "./components/cell-output";

function McpApp() {
  const [content, setContent] = useState<NteractContent | null>(null);

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

  if (!content) return null;

  const cells = content.cells || (content.cell ? [content.cell] : []);
  if (cells.length === 0) return null;

  return (
    <>
      {cells.map((cell) => (
        <CellOutput key={cell.cell_id} cell={cell} />
      ))}
    </>
  );
}

const root = createRoot(document.getElementById("root")!);
root.render(<McpApp />);
