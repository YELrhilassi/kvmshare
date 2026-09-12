import { Keyboard, MonitorUp } from "lucide-react";
import { useApp } from "@/app/AppProvider";
import type { Mode } from "@/lib/bridge";
import { cn } from "@/lib/utils";
import { Section } from "@/components/Section";

// The one place a machine picks its role. The copy is plain language:
// what the user gets, not what the process is called. The selected card
// carries a left accent bar and the live badge — no other decoration.
const OPTIONS: { id: Mode; name: string; what: string; icon: typeof Keyboard }[] = [
  { id: "server", name: "Server", what: "Shares its keyboard and mouse with other machines", icon: Keyboard },
  { id: "client", name: "Client", what: "Lets another machine use its keyboard and mouse", icon: MonitorUp },
];

export default function RolePicker() {
  const { mode, setMode, running } = useApp();
  const active = running[mode];

  return (
    <Section title="What is this machine?">
      <div className="grid gap-2 sm:grid-cols-2">
        {OPTIONS.map((opt) => {
          const on = mode === opt.id;
          const Icon = opt.icon;
          return (
            <button
              key={opt.id}
              onClick={() => void setMode(opt.id)}
              aria-pressed={on}
              className={cn(
                "group relative flex items-start gap-3 rounded-lg border px-4 py-3.5 text-left transition-colors",
                on
                  ? "border-primary/50 bg-muted/50"
                  : "border-border/60 hover:border-border hover:bg-muted/30",
              )}
            >
              <span
                className={cn(
                  "mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-md transition-colors",
                  on ? "bg-primary/15 text-primary" : "bg-muted text-muted-foreground group-hover:text-foreground",
                )}
              >
                <Icon className="h-4 w-4" />
              </span>
              <span className="min-w-0">
                <span className="flex items-center gap-2">
                  <span className={cn("text-sm font-semibold", on ? "text-foreground" : "text-foreground/80")}>
                    {opt.name}
                  </span>
                  {on && active && (
                    <span className="flex items-center gap-1.5 text-[11px] font-medium text-emerald-500">
                      <span className="h-1.5 w-1.5 rounded-full bg-emerald-500" />
                      in use now
                    </span>
                  )}
                </span>
                <span className="mt-0.5 block text-xs leading-relaxed text-muted-foreground">{opt.what}</span>
              </span>
            </button>
          );
        })}
      </div>
      <p className="text-xs text-muted-foreground/70">
        One role at a time — switching stops the other.
      </p>
    </Section>
  );
}
