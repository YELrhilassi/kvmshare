import { useState } from "react";
import { useApp } from "@/app/AppProvider";
import { api } from "@/lib/bridge";
import { Button } from "@/components/ui/button";
import { Section } from "@/components/Section";
import { cn } from "@/lib/utils";

// The single start/stop control for this machine. Everything else that
// shows process state is read-only and points here. For the client the
// label reflects the REAL connection state (the process may run while
// not connected), never just "the process exists".
export default function ShareStatus() {
  const { mode, running, clientState, refresh } = useApp();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const active = running[mode];
  const connected = clientState.status === "connected";
  const verb = mode === "server" ? (active ? "Stop sharing" : "Start sharing") : connected ? "Disconnect" : "Connect";

  const toggle = async () => {
    setBusy(true);
    setError("");
    try {
      if (active) await api().StopActive();
      else await api().StartActive();
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const dot = mode === "server" ? active : connected;
  const title =
    mode === "server"
      ? active
        ? "Sharing this machine"
        : "Not sharing"
      : connected
        ? "Connected"
        : running.client
          ? "Connecting…"
          : "Not connected";

  return (
    <Section title={mode === "server" ? "Sharing" : "Connection"}>
      <div className="flex items-center gap-3">
        <span
          className={cn(
            "h-2.5 w-2.5 rounded-full",
            dot ? "bg-emerald-500" : "bg-muted-foreground/40",
          )}
        />
        <span className="text-xl font-semibold tracking-tight">{title}</span>
      </div>
      <div className="space-y-2">
        <Button onClick={toggle} disabled={busy} variant={dot ? "outline" : "default"} className="w-44">
          {busy ? "Working…" : verb}
        </Button>
        {error && <p className="text-xs text-destructive">{error}</p>}
      </div>
    </Section>
  );
}