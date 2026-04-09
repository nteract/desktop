# MCP App Multi-Cell Renderer

**Date:** 2026-04-08
**Status:** Draft
**PR:** #1662 (backend: `run_all_cells` now returns per-cell outputs)

## Problem

The MCP App output widget (`output.html`) renders cell outputs from MCP tool responses. Currently it handles single-cell and multi-cell identically: every cell is fully expanded with a bare "Source" toggle and raw output dump. This is fine for `execute_cell` (one cell), but for `run_all_cells` returning 10+ cells it produces an overwhelming wall of content where errors (which the AI agent handles) dominate the view while charts and images (which the human is there to see) get lost.

The screenshots from Claude Desktop Cowork showed the problem: the widget rendered long tracebacks prominently while the actual data visualizations were buried below the fold.

## Design Principle

**The MCP App widget is the human's window. The AI agent reads the text content.**

The tool response contains both:
- **Text content items** — full tracebacks, stdout, data summaries. The agent consumes these.
- **Structured content** — rendered in the MCP App widget. The human sees this.

The widget should prioritize what the human uniquely needs — visual outputs they can't get from text — and minimize what the agent already handles.

## Architecture

### Unified cell component

Replace the current `<CellOutput>` with a new `<Cell>` component used for both single-cell and multi-cell responses. It renders:

1. **Cell header row** — always visible, clickable to toggle expand/collapse:
   - Expand/collapse chevron (`▶`/`▼`)
   - Execution count badge (`[3]`)
   - Status indicator (`✓` done, `✗` error, `⊘` cancelled)
   - Preview text (first line of primary text output, truncated with ellipsis)

2. **Cell body** — shown when expanded:
   - Outputs rendered with existing `StreamOutput`, `ErrorOutput`, `MimeRenderer` components
   - Collapsible "Source" toggle (existing behavior, preserved)

### Content-aware collapse

Each cell's default expand/collapse state is determined by its output content types:

| Content type | Default state | Rationale |
|-------------|---------------|-----------|
| Images (`image/*`) | **Expanded** | Visual — human needs to see |
| Plotly (`application/vnd.plotly.v1+json`) | **Expanded** | Visual |
| Vega/Vega-Lite | **Expanded** | Visual |
| Leaflet/GeoJSON (`application/geo+json`) | **Expanded** | Visual |
| HTML (`text/html`) | **Expanded** | Visual (tables, styled output) |
| Markdown (`text/markdown`) | **Expanded** | Visual (formatted text) |
| SVG (`image/svg+xml`) | **Expanded** | Visual |
| Errors | **Collapsed** | Agent has the traceback |
| Stream stdout/stderr | **Collapsed** | Agent reads text |
| text/plain | **Collapsed** | Agent reads text |
| JSON | **Collapsed** | Agent reads text |
| Cancelled | **Collapsed** | Nothing to show |

**Decision logic:** A cell is expanded if _any_ of its outputs contain a rich MIME type. The check looks at the output manifests' `data` keys (for `display_data`/`execute_result`) or `output_type` (for errors/streams).

A utility function `hasRichOutput(cell: CellData): boolean` checks whether any output in the cell has a MIME type that should be expanded.

### Summary header (multi-cell only)

When `cells.length > 1`, render a summary bar above the cell list:

```
✓ 8 succeeded · ✗ 1 errored · ⊘ 1 cancelled     [Expand all]
```

- Status counts derived from `cell.status` field
- "Expand all" / "Collapse all" toggle in the top-right

For single-cell responses (`cells.length === 1` or `cell` field), no summary header. The cell is always expanded.

### Preview text for collapsed cells

The collapsed row shows a one-line preview extracted from the cell's outputs:

1. If the cell has `text/llm+plain` in any output → use that (it's the AI-synthesized summary)
2. Else if the cell has `text/plain` in any `display_data`/`execute_result` → use first line
3. Else if the cell has stream `stdout` text → use first line
4. Else if the cell has an error → show `ename: evalue` (e.g., "ModuleNotFoundError: No module named 'pandas'")
5. Else → show status text ("done", "cancelled")

Preview text is truncated with CSS `text-overflow: ellipsis`.

### Live polling via `callServerTool`

The MCP Apps SDK (`@modelcontextprotocol/ext-apps` v1.3.2) provides `app.callServerTool()` — the widget can call MCP tools back through the host proxy. This enables live updates:

- When a cell's `status` is `"running"` or `"queued"`, the widget can poll `get_cell` with the cell's `cell_id` to fetch updated outputs and status.
- When the cell reaches terminal status (`"done"` or `"error"`), polling stops.
- This turns the widget from a static snapshot into a **live view** of execution progress.

**Polling strategy:**
- Only poll cells that aren't in terminal status
- Poll interval: ~1-2s (avoid flooding the server)
- Stop polling when all cells reach terminal status
- Update cell outputs in-place as they arrive

**Schema addition:** Include `execution_id` per cell in structured content so the widget can track specific executions. Add to `CellData`:
```ts
execution_id?: string;
```

This is a stretch goal — the initial implementation can ship without polling and add it as a follow-up.

### Remove 0px collapse for cell content

The current `useCollapseWhenEmpty` collapses the entire widget to 0px when there are no outputs. With the new cell headers showing execution count + status, the widget has useful content to show even without outputs.

**New behavior:**
- If structured content has cells (even with empty outputs) → show cell headers
- Only collapse to 0px when there is truly no structured content (e.g., `replace_match` returning no cell data)

### Structured content schema

No changes to the core schema. The existing shapes work:

- Single-cell: `{"cell": {"cell_id": "...", "outputs": [...], ...}, "blob_base_url": "..."}`
- Multi-cell: `{"cells": [{"cell_id": "...", ...}, ...], "blob_base_url": "..."}`

The `NteractContent` type already supports both via `cell?` / `cells?` fields, and `mcp-app.tsx` normalizes them to an array.

## Component Changes

### `mcp-app.tsx`

- Pass `isMultiCell={cells.length > 1}` to control summary header rendering
- Render `<SummaryHeader>` when multi-cell
- Replace `<CellOutput>` mapping with `<Cell>` mapping

### New: `components/summary-header.tsx`

- Counts cells by status
- Renders status line with counts and icons
- "Expand all" / "Collapse all" toggle button
- Manages `allExpanded` state, passes override to cells

### Revised: `components/cell-output.tsx` → `components/cell.tsx`

- Rename to `Cell` to reflect broader role
- Add `defaultExpanded` prop (derived from `hasRichOutput`)
- Add `forceExpanded` prop (from summary header's expand-all toggle)
- Cell header row: execution count, status icon, preview text, chevron
- Click header to toggle expand/collapse
- Body: existing output rendering (unchanged internally)

### New: `lib/rich-output.ts`

- `hasRichOutput(cell: CellData): boolean` — checks output MIME types
- `getPreviewText(cell: CellData): string` — extracts one-line preview
- `RICH_MIME_TYPES: Set<string>` — the set of MIME types that trigger expansion

### Unchanged

- `mime-renderer.tsx`, `error-output.tsx`, `stream-output.tsx`, `ansi-text.tsx`, `html-output.tsx`, `image-output.tsx`, `json-output.tsx`, `code-block.tsx` — all internal output renderers stay as-is
- Plugin loading, blob fetching — unchanged
- `types.ts` — no schema changes needed
- `style.css` — add new styles for cell header, summary header, collapse states

## Styling

New CSS additions to `style.css`:

- `.cell-header` — the clickable header row (flex, align-items center, gap, cursor pointer, hover state)
- `.cell-header .execution-count` — monospace badge
- `.cell-header .status-icon` — status indicator colors
- `.cell-header .preview-text` — truncated one-line preview (flex: 1, overflow hidden, ellipsis)
- `.cell-body` — the expandable content area
- `.summary-header` — top bar with counts and expand-all toggle
- Existing `.cell`, `.outputs`, `.source-details` styles preserved

## Testing

- Verify single-cell `execute_cell` renders with new cell header (always expanded, no summary)
- Verify multi-cell `run_all_cells` shows summary header with correct counts
- Verify rich-content cells (plotly, images) auto-expand
- Verify text-only and error cells auto-collapse with preview
- Verify clicking collapsed cell expands it
- Verify "Expand all" / "Collapse all" toggle works
- Verify widget shows cell headers even when outputs are empty (no 0px collapse)
- Verify widget still collapses to 0px when no structured content at all (e.g., `replace_match`)
