// One selectable routing-target button, used by both the category
// picker and the pinned-machine grid: identical card, identical active
// treatment, two data sources. Composed rather than copy-pasted so the
// two can never drift apart visually.

import { cn } from "@/lib/utils";

export function TargetCard({
  label,
  hint,
  active,
  onClick,
}: {
  label: string;
  hint: string;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className={cn(
        "rounded-lg border px-3 py-2.5 text-left transition-colors",
        active ? "border-primary bg-primary/5" : "border-border/70 hover:border-border hover:bg-muted/30",
      )}
    >
      <div className={cn("text-sm font-medium", active && "text-primary")}>{label}</div>
      <div className="mt-0.5 text-[11px] leading-snug text-muted-foreground">{hint}</div>
    </button>
  );
}
