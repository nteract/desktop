import { createRoot } from "react-dom/client";
import { useEffect, useRef, useState } from "react";
import { AppBridge, PostMessageTransport } from "@modelcontextprotocol/ext-apps/app-bridge";
import type { CallToolResult } from "@modelcontextprotocol/sdk/types.js";
import type { CellData, NteractContent } from "../types";
import {
  singleCellPlotly,
  singleCellError,
  singleCellText,
  singleCellImage,
  multiCellRun,
} from "./fixtures";

// Set dark theme for host page
document.documentElement.setAttribute("data-theme", "dark");

/** Build a CallToolResult with structured content, matching what runt-mcp produces. */
function makeToolResult(cells: CellData | CellData[], blobBaseUrl?: string): CallToolResult {
  const structured: NteractContent = Array.isArray(cells)
    ? { cells, blob_base_url: blobBaseUrl }
    : { cell: cells, blob_base_url: blobBaseUrl };

  return {
    content: [{ type: "text", text: "Output rendered in widget" }],
    structuredContent: structured as Record<string, unknown>,
  };
}

/**
 * An iframe that loads the real mcp-app entry point and communicates
 * via the MCP Apps JSON-RPC protocol (AppBridge + PostMessageTransport).
 */
function AppFrame({ toolResult }: { toolResult: CallToolResult }) {
  const iframeRef = useRef<HTMLIFrameElement>(null);
  const bridgeRef = useRef<AppBridge | null>(null);
  const [height, setHeight] = useState(40);

  useEffect(() => {
    const iframe = iframeRef.current;
    if (!iframe) return;

    const win = iframe.contentWindow;
    if (!win) return;

    // Create AppBridge BEFORE iframe finishes loading so we catch the
    // App's initialize request. contentWindow exists as soon as the
    // iframe element is in the DOM.
    const bridge = new AppBridge(
      null,
      { name: "nteract-dev-preview", version: "0.1.0" },
      {},
      {
        hostContext: {
          theme: "dark",
        },
      },
    );
    bridgeRef.current = bridge;

    // Resize iframe to match content
    bridge.onsizechange = ({ height: h }) => {
      if (h != null) setHeight(h);
    };

    bridge.oninitialized = () => {
      bridge.sendToolInput({ arguments: {} });
      bridge.sendToolResult(toolResult);
    };

    const transport = new PostMessageTransport(win, win);
    bridge.connect(transport);

    return () => {
      bridge.close();
    };
  }, [toolResult]);

  return (
    <iframe
      ref={iframeRef}
      className="app-frame"
      src="/dev/app.html"
      style={{ height: `${height}px` }}
      title="MCP App"
    />
  );
}

function ToolCall({ name, args, result }: { name: string; args?: string; result: CallToolResult }) {
  return (
    <div className="tool-call">
      <div className="tool-call-header">
        <span className="tool-icon">N</span>
        <span>
          nteract {name}
          {args ? ` ${args}` : ""}
        </span>
      </div>
      <AppFrame toolResult={result} />
    </div>
  );
}

function DevPreview() {
  return (
    <>
      <h1>nteract MCP App — Dev Preview</h1>

      <div className="turn">
        <div className="turn-label">Claude</div>
        <div className="message">
          Let me run the scatter plot to visualize the gap distribution.
        </div>
      </div>
      <ToolCall
        name="execute_cell"
        args='cell_id="cell-a1b2c3d4"'
        result={makeToolResult(singleCellPlotly)}
      />

      <div className="turn">
        <div className="turn-label">Claude</div>
        <div className="message">Let me import the dependencies first.</div>
      </div>
      <ToolCall
        name="execute_cell"
        args='cell_id="cell-e5f6g7h8"'
        result={makeToolResult(singleCellError)}
      />

      <div className="turn">
        <div className="turn-label">Claude</div>
        <div className="message">Let me run all cells from the top to rebuild everything.</div>
      </div>
      <ToolCall name="run_all_cells" result={makeToolResult(multiCellRun)} />

      <div className="turn">
        <div className="turn-label">Claude</div>
        <div className="message">Here are the summary statistics.</div>
      </div>
      <ToolCall
        name="execute_cell"
        args='cell_id="cell-t1"'
        result={makeToolResult(singleCellText)}
      />

      <div className="turn">
        <div className="turn-label">Claude</div>
        <div className="message">And the line chart.</div>
      </div>
      <ToolCall
        name="execute_cell"
        args='cell_id="cell-img1"'
        result={makeToolResult(singleCellImage)}
      />
    </>
  );
}

const root = createRoot(document.getElementById("root")!);
root.render(<DevPreview />);
