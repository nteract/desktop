/**
 * Skeleton placeholder shown while the notebook document is loading from the daemon.
 *
 * Renders several placeholder cells with varying heights to mimic a real notebook.
 * The staggered animation delays give a subtle wave effect while loading.
 */

const skeletonCells = [
  { height: "2.5rem", delay: "0ms" },
  { height: "5rem", delay: "75ms" },
  { height: "2rem", delay: "150ms" },
];

function SkeletonCell({ height, delay }: { height: string; delay: string }) {
  return (
    <div className="flex py-4">
      {/* Gutter area — matches CellContainer's w-10 gutter */}
      <div className="w-10 flex-shrink-0" />

      {/* Ribbon — self-stretch to fill container height */}
      <div className="w-1 self-stretch rounded-sm bg-muted/40" />

      {/* Editor area placeholder */}
      <div className="min-w-0 flex-1 py-3 pl-6 pr-3">
        <div
          className="rounded bg-muted/30 animate-pulse"
          style={{ minHeight: height, animationDelay: delay }}
        />
      </div>
    </div>
  );
}

export function CellSkeleton() {
  return (
    <div>
      {skeletonCells.map((cell, i) => (
        <SkeletonCell key={i} height={cell.height} delay={cell.delay} />
      ))}
    </div>
  );
}
