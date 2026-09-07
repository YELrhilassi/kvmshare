import { useApp } from "@/app/AppProvider";
import type { Mode } from "@/lib/bridge";
import { cn } from "@/lib/utils";
import { Section } from "@/components/Section";

// The one place a machine picks its role. The copy is plain language:
// what the user gets, not what the process is called.
const OPTIONS: { id: Mode; name: string; what: string }[] = [
  { id: "server", name: "Server", what: "Share this machine's keyboard and mouse" },
  { id: "client", name: "Client", what: "Control this machine from another computer" },
];

export default function RolePicker() {
  const { mode, setMode, running } = useApp();
  const active = running[mode];

  return (
    <Section title="How this machine is used">
      <div className="grid gap-px overflow-hidden rounded-md border border-border/70 bg-border/70 sm:grid-cols-2">
        {OPTIONS.map((opt) => {
          const on = mode === opt.id;
          return (
            <button
              key={opt.id}
              onClick={() => void setMode(opt.id)}
              className={cn(
                "group flex flex-col items-start gap-1 bg-background px-5 py-4 text-left transition-colors",
                on ? "bg-muted/60" : "hover:bg-muted/30",
              )}
            >
              <span
                className={cn(
                  "text-sm font-semibold transition-colors",
                  on ? "text-foreground" : "text-muted-foreground group-hover:text-foreground",
                )}
              >
                {opt.name}
              </span>
              <span className="text-xs leading-relaxed text-muted-foreground">{opt.what}</span>
              {on && active && (
                <span className="mt-1 text-[11px] font-medium text-emerald-500">in use now</span>
              )}
            </button>
          );
        })}
      </div>
      <p className="text-xs text-muted-foreground/70">
        A machine runs as one role at a time — switching stops the running process.
      </p>
    </Section>
  );
}