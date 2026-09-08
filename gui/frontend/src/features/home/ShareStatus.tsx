import { useEffect, useState } from "react";
import { useApp } from "@/app/AppProvider";
import { api } from "@/lib/bridge";
import { Button } from "@/components/ui/button";
import { Section } from "@/components/Section";
import { cn } from "@/lib/utils";
import { DEFAULT_PORT } from "@/lib/constants";

// The single status + control for this machine, written as a sentence
// about direction: a server shares with machines that connect TO it; a
// client is connected TO exactly one server (by name when discovery
// knows it). Bare "Connected" never appears without a "to whom".
export default function ShareStatus() {
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

  // Name the server this client talks to, when discovery knows it —
  // otherwise fall back to its address.
  const serverPeer = peers.find(
    (p) => p.role === "server" && `${p.addr}:${p.port || DEFAULT_PORT}` === clientState.server,
  );
  const serverLabel = serverPeer?.name || clientState.server || "a server";

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
      ? `Connected to ${serverLabel}`
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
      ? `${serverLabel} is using this machine's keyboard and mouse${clientName ? ` (as ${clientName})` : ""}.`
      : connecting
        ? stuckConnecting
          ? `Can't reach ${serverLabel} — retrying every few seconds.`
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
    <Section title="Status">
      <div className="flex items-center gap-3">
        <span className={cn("h-2.5 w-2.5 rounded-full", dot ? "bg-emerald-500" : "bg-muted-foreground/40")} />
        <span className="text-xl font-semibold tracking-tight">{title}</span>
      </div>
      <p className="max-w-md text-sm text-muted-foreground">{blurb}</p>
      <div className="space-y-2">
        <Button onClick={toggle} disabled={busy} variant={dot ? "outline" : "default"} className="w-44">
          {busy ? "Working…" : verb}
        </Button>
        {error && <p className="text-xs text-destructive">{error}</p>}
      </div>
    </Section>
  );
}