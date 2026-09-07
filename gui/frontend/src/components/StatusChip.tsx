import { cn } from "@/lib/utils";

// A small live-state pill: dot + plain-language word. Used by Server and
// Client pages so the status readout is identical everywhere.
export default function StatusChip({
  active,
  activeLabel,
  idleLabel,
}: {
  active: boolean;
  activeLabel: string;
  idleLabel: string;
}) {
  return (
    <span className="inline-flex items-center gap-1.5 rounded-full border border-border/70 px-2.5 py-1 text-xs text-muted-foreground">
      <span
        className={cn("h-1.5 w-1.5 rounded-full", active ? "bg-emerald-500" : "bg-muted-foreground/40")}
      />
      {active ? activeLabel : idleLabel}
    </span>
  );
}