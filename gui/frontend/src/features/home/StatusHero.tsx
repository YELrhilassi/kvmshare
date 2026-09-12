import { useEffect, useState } from "react";
import { useApp } from "@/app/AppProvider";
import { api } from "@/lib/bridge";
import { Button } from "@/components/ui/button";
import DeskMap from "@/features/home/DeskMap";
import { cn } from "@/lib/utils";

/**
 * The hero: one glance answers "what is happening right now" — a big
 * status sentence, the single control that changes it, and the live
 * desk map beside it. Everything else on the page is detail.
 */
export default function StatusHero() {
  const { mode, running, clientState, clientName, peers, refresh } = useApp();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  // A wall-clock tick while connecting, so the "Connecting… → Server
  // unreachable" transition happens on its own (the backend only
  // emits events on state *changes*, and a stuck connection changes
  // nothing). Stopped when nothing is connecting.
  const [now, setNow] = useState(() => Date.now());
  const connectingActive = !(mode === "server") && running.client && clientState.status !== "connected";
  useEffect(() => {
    if (!connectingActive) return;
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [connectingActive]);

  const isServer = mode === "server";
  const active = isServer ? running.server : running.client;
  const connected = clientState.status === "connected";
  const connecting = !isServer && running.client && !connected;
  // A client that has been "connecting" past a short grace period is
  // not really on its way — the server is unreachable and it keeps
  // retrying. Say that instead of a forever-"Connecting…".
  const stuckConnecting =
    connecting &&
    clientState.connectingSinceMs > 0 &&
    now - clientState.connectingSinceMs > 8_000;

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

  const dot = isServer ? active : connected;
  const title = isServer
    ? active
      ? "Sharing"
      : "Not sharing"
    : connected
      ? "In use by your server"
      : connecting
        ? stuckConnecting
          ? "Server unreachable"
          : "Connecting…"
        : "Not connected";

  const blurb = isServer
    ? active
      ? "Your keyboard and mouse are shared — other machines can use them."
      : "Nothing is shared right now. Start to let other machines use your keyboard and mouse."
    : connected
      ? `Your server is using this machine's keyboard and mouse${clientName ? ` (as ${clientName})` : ""}.`
      : connecting
        ? stuckConnecting
          ? `Can't reach the server — retrying every few seconds.`
          : "Connecting to the server — nothing is shared yet."
        : "No one is controlling this machine right now. Pick a machine below to connect.";

  const verb = isServer
    ? active
      ? "Stop sharing"
      : "Start sharing"
    : connected
      ? "Disconnect"
      : connecting
        ? "Stop"
        : "Connect";

  return (
    <section className="overflow-hidden rounded-xl border border-border/70 bg-muted/20">
      <div className="grid gap-8 p-8 lg:grid-cols-[minmax(0,1fr)_minmax(0,1.15fr)] lg:gap-10 lg:p-10">
        <div className="flex min-w-0 flex-col items-start gap-4">
          <div className="flex items-center gap-3">
            <span className={cn("h-3 w-3 rounded-full", dot ? "bg-emerald-500" : connecting ? "animate-pulse bg-amber-500" : "bg-muted-foreground/40")} />
            <h1 className="text-3xl font-semibold tracking-tight">{title}</h1>
          </div>
          <p className="max-w-md text-sm leading-relaxed text-muted-foreground">{blurb}</p>
          <div className="flex flex-wrap items-center gap-3 pt-1">
            <Button onClick={toggle} disabled={busy} variant={dot ? "outline" : "default"} className="w-44">
              {busy ? "Working…" : verb}
            </Button>
            {error && <p className="text-xs text-destructive">{error}</p>}
          </div>
        </div>

        <div className="min-w-0 self-center" aria-hidden={peers.length === 0 && !running.client && !running.server}>
          <DeskMap />
        </div>
      </div>
    </section>
  );
}
