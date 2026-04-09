import type { CellData } from "../types";

/** MIME types that represent rich visual content the human needs to see. */
const RICH_MIME_TYPES = new Set([
	"image/png",
	"image/jpeg",
	"image/gif",
	"image/webp",
	"image/svg+xml",
	"text/html",
	"text/markdown",
	"text/latex",
	"application/vnd.plotly.v1+json",
	"application/geo+json",
]);

/** Prefix patterns for vega/vegalite (versioned MIME types). */
const RICH_MIME_PREFIXES = [
	"application/vnd.vegalite.v",
	"application/vnd.vega.v",
];

function isRichMime(mime: string): boolean {
	if (RICH_MIME_TYPES.has(mime)) return true;
	return RICH_MIME_PREFIXES.some((p) => mime.startsWith(p));
}

/** Whether a cell has any output that should be visually expanded. */
export function hasRichOutput(cell: CellData): boolean {
	if (!cell.outputs?.length) return false;
	return cell.outputs.some((output) => {
		if (output.output_type === "display_data" || output.output_type === "execute_result") {
			if (!output.data) return false;
			return Object.keys(output.data).some(isRichMime);
		}
		return false;
	});
}

/** Extract a one-line preview string for a collapsed cell. */
export function getPreviewText(cell: CellData): string {
	for (const output of cell.outputs ?? []) {
		// Prefer text/llm+plain (AI-synthesized summary)
		if (output.data?.["text/llm+plain"]) {
			return firstLine(String(output.data["text/llm+plain"]));
		}
	}

	for (const output of cell.outputs ?? []) {
		// text/plain from display_data or execute_result
		if (
			(output.output_type === "display_data" || output.output_type === "execute_result") &&
			output.data?.["text/plain"]
		) {
			return firstLine(String(output.data["text/plain"]));
		}
	}

	for (const output of cell.outputs ?? []) {
		// Stream stdout
		if (output.output_type === "stream" && output.text) {
			return firstLine(String(output.text));
		}
	}

	for (const output of cell.outputs ?? []) {
		// Error
		if (output.output_type === "error") {
			const name = output.ename || "Error";
			const value = output.evalue || "";
			return value ? `${name}: ${value}` : name;
		}
	}

	return cell.status || "";
}

function firstLine(text: string): string {
	const line = text.split("\n")[0]?.trim() ?? "";
	return line;
}
