import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

// A page section: a small uppercase heading over a hairline rule, then
// the content. No boxes — just type, rules and whitespace.
export function Section({
  title,
  action,
  children,
  className,
}: {
  title: string;
  action?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={cn("space-y-6", className)}>
      <div className="flex items-center justify-between border-b border-border/70 pb-2">
        <h2 className="text-xs font-semibold tracking-widest text-muted-foreground uppercase">
          {title}
        </h2>
        {action}
      </div>
      {children}
    </section>
  );
}

// One row of label + value, separated by a hairline.
export function Row({
  label,
  value,
  mono,
}: {
  label: string;
  value: string;
  mono?: boolean;
}) {
  return (
    <div className="flex items-baseline justify-between gap-6 border-b border-border/50 py-3 last:border-b-0">
      <span className="text-sm text-muted-foreground">{label}</span>
      <span className={cn("text-sm", mono && "font-mono")}>{value}</span>
    </div>
  );
}