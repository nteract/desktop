import { App, applyDocumentTheme, type McpUiHostContext } from "@modelcontextprotocol/ext-apps";
import type { CallToolResult } from "@modelcontextprotocol/sdk/types.js";

const root = document.getElementById("root")!;

// ── Output types matching structuredContent from nteract MCP ────────

interface CellOutput {
  output_type: "stream" | "error" | "display_data" | "execute_result";
  name?: string;
  text?: string;
  ename?: string;
  evalue?: string;
  traceback?: string[];
  data?: Record<string, string>;
}

interface CellData {
  cell_id: string;
  source: string;
  outputs: CellOutput[];
  execution_count: number | null;
  status: string;
}

interface NteractContent {
  cell?: CellData;
  cells?: CellData[];
}

// ── Rendering ───────────────────────────────────────────────────────

function stripAnsi(text: string): string {
  return text.replace(/\x1b\[[0-9;]*[A-Za-z]|\x1b\].*?\x07|\x1b\(B/g, "");
}

function renderOutput(output: CellOutput): HTMLElement {
  const el = document.createElement("div");
  el.className = "output";

  if (output.output_type === "stream") {
    const pre = document.createElement("div");
    pre.className = "stream" + (output.name === "stderr" ? " stream-stderr" : "");
    pre.textContent = stripAnsi(output.text || "");
    el.appendChild(pre);
  } else if (output.output_type === "error") {
    const pre = document.createElement("div");
    pre.className = "error-output";
    const parts: string[] = [];
    if (output.ename) parts.push(`${output.ename}: ${output.evalue || ""}`);
    if (output.traceback?.length) parts.push(output.traceback.map(stripAnsi).join("\n"));
    pre.textContent = parts.join("\n\n");
    el.appendChild(pre);
  } else if (output.output_type === "display_data" || output.output_type === "execute_result") {
    const data = output.data || {};
    const imgMime = ["image/png", "image/jpeg", "image/gif", "image/webp"].find(m => data[m]);
    if (imgMime) {
      const img = document.createElement("img");
      img.className = "image-output";
      const raw = data[imgMime];
      img.src = raw.startsWith("data:") || raw.startsWith("http") ? raw : `data:${imgMime};base64,${raw}`;
      if (data["text/plain"]) img.alt = data["text/plain"];
      el.appendChild(img);
    } else if (data["text/html"]) {
      const frame = document.createElement("iframe");
      frame.className = "html-output";
      frame.sandbox.add("allow-scripts");
      // Inject theme-aware styles into the iframe so HTML outputs
      // (e.g. pandas DataFrames) render with the correct colors.
      const styles = getComputedStyle(document.documentElement);
      const bg = styles.getPropertyValue("--bg").trim() || "#1e1e1e";
      const fg = styles.getPropertyValue("--fg").trim() || "#e5e5e5";
      const border = styles.getPropertyValue("--border").trim() || "#374151";
      const codeBg = styles.getPropertyValue("--code-bg").trim() || "#262626";
      const fgMuted = styles.getPropertyValue("--fg-muted").trim() || "#9ca3af";
      frame.srcdoc = `<!DOCTYPE html><html><head><style>
        * { box-sizing: border-box; margin: 0; padding: 0; }
        body { font-family: system-ui, -apple-system, sans-serif; color: ${fg}; background: ${bg}; }
        table { border-collapse: collapse; font-family: ui-monospace, SFMono-Regular, monospace; font-size: 13px; }
        th, td { padding: 6px 10px; border: 1px solid ${border}; text-align: left; }
        th { background: ${codeBg}; color: ${fgMuted}; font-weight: 600; }
        tr:hover td { background: ${codeBg}; }
      </style></head><body>${data["text/html"]}</body></html>`;
      frame.addEventListener("load", () => {
        try {
          const h = frame.contentDocument?.documentElement?.scrollHeight;
          if (h) frame.style.height = `${h + 2}px`;
        } catch { /* cross-origin, ignore */ }
      });
      el.appendChild(frame);
    } else if (data["application/json"]) {
      const pre = document.createElement("div");
      pre.className = "display-text";
      const jsonData = data["application/json"];
      try {
        const obj = typeof jsonData === "string" ? JSON.parse(jsonData) : jsonData;
        pre.textContent = JSON.stringify(obj, null, 2);
      } catch {
        pre.textContent = String(jsonData);
      }
      el.appendChild(pre);
    } else if (data["text/plain"]) {
      const pre = document.createElement("div");
      pre.className = "display-text";
      pre.textContent = data["text/plain"];
      el.appendChild(pre);
    }
  }
  return el;
}

function renderCell(cell: CellData): HTMLElement {
  const cellEl = document.createElement("div");
  cellEl.className = "cell";

  // Source (collapsed by default)
  if (cell.source) {
    const details = document.createElement("details");
    details.className = "source-details";
    const summary = document.createElement("summary");
    summary.className = "source-summary";
    summary.textContent = "Source";
    details.appendChild(summary);
    const src = document.createElement("pre");
    src.className = "source";
    src.textContent = cell.source;
    details.appendChild(src);
    cellEl.appendChild(details);
  }

  // Outputs
  if (cell.outputs?.length) {
    const outputsEl = document.createElement("div");
    outputsEl.className = "outputs";
    for (const output of cell.outputs) {
      outputsEl.appendChild(renderOutput(output));
    }
    cellEl.appendChild(outputsEl);
  }

  return cellEl;
}

function render(data: NteractContent) {
  root.innerHTML = "";
  const cells = data.cells || (data.cell ? [data.cell] : []);
  if (cells.length === 0) {
    root.innerHTML = '<div class="empty-state">No output</div>';
    return;
  }
  for (const cell of cells) {
    root.appendChild(renderCell(cell));
  }
}

// ── MCP Apps connection ─────────────────────────────────────────────

const app = new App({ name: "nteract", version: "0.1.0" });

app.ontoolresult = (result: CallToolResult) => {
  const content = result.structuredContent as NteractContent | undefined;
  render(content ?? {});
};

app.onhostcontextchanged = (ctx: McpUiHostContext) => {
  if (ctx.theme) applyDocumentTheme(ctx.theme);
};

app.onerror = console.error;

app.connect().then(() => {
  root.textContent = "Connected — waiting for execution...";
});
