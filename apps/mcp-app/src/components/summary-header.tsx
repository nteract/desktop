import type { CellData } from "../types";

interface SummaryHeaderProps {
  cells: CellData[];
  allExpanded: boolean;
  onToggleAll: () => void;
}

export function SummaryHeader({ cells, allExpanded, onToggleAll }: SummaryHeaderProps) {
  let succeeded = 0;
  let errored = 0;
  let cancelled = 0;

  for (const cell of cells) {
    switch (cell.status) {
      case "done":
        succeeded++;
        break;
      case "error":
        errored++;
        break;
      case "cancelled":
        cancelled++;
        break;
    }
  }

  const parts: string[] = [];
  if (succeeded > 0) parts.push(`✓ ${succeeded} succeeded`);
  if (errored > 0) parts.push(`✗ ${errored} errored`);
  if (cancelled > 0) parts.push(`⊘ ${cancelled} cancelled`);

  return (
    <div className="summary-header">
      <span className="summary-counts">{parts.join(" · ")}</span>
      <button type="button" className="summary-toggle" onClick={onToggleAll}>
        {allExpanded ? "Collapse all" : "Expand all"}
      </button>
    </div>
  );
}
