import { createRoot } from "react-dom/client";
import { useState } from "react";
import "../style.css";
import { Cell } from "../components/cell";
import { SummaryHeader } from "../components/summary-header";
import { hasRichOutput } from "../lib/rich-output";
import type { CellData } from "../types";
import {
	singleCellPlotly,
	singleCellError,
	singleCellText,
	singleCellImage,
	multiCellRun,
} from "./fixtures";

/** Simulates how a single tool call result looks in a chat. */
function ToolResult({
	toolName,
	args,
	cells,
}: { toolName: string; args?: string; cells: CellData[] }) {
	const isMultiCell = cells.length > 1;
	const [allExpanded, setAllExpanded] = useState<boolean | null>(null);

	return (
		<div className="tool-call">
			<div className="tool-call-header">
				<span className="tool-icon">⚡</span>
				<span>
					{toolName}
					{args ? ` ${args}` : ""}
				</span>
			</div>
			<div className="tool-call-body">
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
						defaultExpanded={!isMultiCell || hasRichOutput(cell)}
						forceExpanded={isMultiCell ? allExpanded : null}
					/>
				))}
			</div>
		</div>
	);
}

function DevPreview() {
	return (
		<>
			{/* ── Single cell: execute_cell with plotly ── */}
			<div className="turn">
				<div className="turn-label assistant">Claude</div>
				<div className="message">
					Let me run the scatter plot to visualize the gap distribution.
				</div>
			</div>
			<ToolResult toolName="execute_cell" args='cell_id="cell-a1b2c3d4"' cells={[singleCellPlotly]} />

			{/* ── Single cell: error ── */}
			<div className="turn">
				<div className="turn-label assistant">Claude</div>
				<div className="message">Let me import the dependencies first.</div>
			</div>
			<ToolResult toolName="execute_cell" args='cell_id="cell-e5f6g7h8"' cells={[singleCellError]} />

			{/* ── Multi-cell: run_all_cells ── */}
			<div className="turn">
				<div className="turn-label assistant">Claude</div>
				<div className="message">
					Let me run all cells from the top to rebuild everything.
				</div>
			</div>
			<ToolResult toolName="run_all_cells" cells={multiCellRun} />

			{/* ── Single cell: text-only with HTML table ── */}
			<div className="turn">
				<div className="turn-label assistant">Claude</div>
				<div className="message">Here are the summary statistics.</div>
			</div>
			<ToolResult toolName="execute_cell" args='cell_id="cell-t1"' cells={[singleCellText]} />

			{/* ── Single cell: image output ── */}
			<div className="turn">
				<div className="turn-label assistant">Claude</div>
				<div className="message">And the line chart for the time series.</div>
			</div>
			<ToolResult toolName="execute_cell" args='cell_id="cell-img1"' cells={[singleCellImage]} />
		</>
	);
}

const root = createRoot(document.getElementById("root")!);
root.render(<DevPreview />);
