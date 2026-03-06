/**
 * Skeleton placeholder shown while the notebook is loading.
 * Mimics the visual structure of a code cell with AddCellButtons spacing.
 */
export function CellSkeleton() {
  return (
    <div className="flex py-4">
      {/* Gutter area — matches CellContainer's w-10 gutter */}
      <div className="w-10 flex-shrink-0" />

      {/* Ribbon — self-stretch to fill container height */}
      <div className="w-1 self-stretch bg-gray-200 dark:bg-gray-700" />

      {/* Editor area placeholder */}
      <div className="min-w-0 flex-1 py-3 pl-6 pr-3">
        <div className="min-h-[2rem] rounded bg-muted/50 animate-pulse" />
      </div>
    </div>
  );
}
