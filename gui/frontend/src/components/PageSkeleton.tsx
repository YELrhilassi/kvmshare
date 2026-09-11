import { cn } from "@/lib/utils";

// Page-level loading skeletons: every page shows the same shape while its
// config round-trips, so navigation never flashes an empty shell or a bare
// "Loading…" line. Skeleton rows mirror the page's real rhythm (title,
// sections, control rows) at low fidelity — no spinners, no layout jump.

function Bar({ className }: { className?: string }) {
  return <div className={cn("h-4 animate-pulse rounded bg-muted", className)} />;
}

/** Skeleton for the standard page: title + subtitle, then Sections. */
export function PageSkeleton({ rows = 3 }: { rows?: number }) {
  return (
    <div className="mx-auto w-full max-w-2xl px-8 py-8" aria-busy="true" aria-live="polite">
      <Bar className="h-5 w-40" />
      <Bar className="mt-2 h-3.5 w-72" />
      {Array.from({ length: rows }, (_, i) => (
        <div key={i} className="mt-6 rounded-xl border border-border/50 p-4">
          <Bar className="h-3.5 w-24" />
          {Array.from({ length: 2 }, (_, j) => (
            <div key={j} className="mt-4 flex items-center justify-between gap-6">
              <div className="flex-1 space-y-2">
                <Bar className="h-3.5 w-48" />
                <Bar className="h-3 w-64" />
              </div>
              <Bar className="h-5 w-9 rounded-full" />
            </div>
          ))}
        </div>
      ))}
      <span className="sr-only">Loading…</span>
    </div>
  );
}
