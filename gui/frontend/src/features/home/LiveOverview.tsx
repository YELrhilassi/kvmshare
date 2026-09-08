import { useEffect, useState } from "react";
import { useApp } from "@/app/AppProvider";
import { api, type ConnectedClient, type Peer, type Settings } from "@/lib/bridge";
import { DEFAULT_PORT } from "@/lib/constants";
import { Button } from "@/components/ui/button";
import { Section } from "@/components/Section";
import { cn, shortID } from "@/lib/utils";

// The live half of the dashboard. A server shows who is connected and
// lets the operator control those machines; a client shows its REAL
// connection state (from client.state) and which servers are nearby.
// Nothing here is configuration — that lives on the config pages.

function ConnectionLine({
  status,
  server,
}: {
  status: "connected" | "connecting" | "disconnected";
  server: string;
}) {
  const label =
    status === "connected" ? "Connected" : status === "connecting" ? "Connecting…" : "Not connected";
  return (
    <div className="flex items-center gap-3 py-3">
      <span
        className={cn(
          "h-2 w-2 rounded-full",
          status === "connected" ? "bg-emerald-500" : status === "connecting" ? "bg-amber-500" : "bg-muted-foreground/40",
        )}
      />
      <div className="min-w-0">
        <div className="text-sm font-medium">{label}</div>
        {server && <div className="font-mono text-[11px] text-muted-foreground/60">to {server}</div>}
      </div>
    </div>
  );
}

export default function LiveOverview() {
  const { mode, clientState } = useApp();
  const [clients, setClients] = useState<ConnectedClient[]>([]);
  const [peers, setPeers] = useState<Peer[]>([]);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [err, setErr] = useState("");

  useEffect(() => {
    const tick = () => {
      void api()
        .ListClients()
        .then(setClients)
        .catch(() => {});
      void api()
        .DiscoverPeers()
        .then(setPeers)
        .catch(() => {});
      void api()
        .GetSettings()
        .then(setSettings)
        .catch(() => {});
    };
    void tick();
    const id = setInterval(tick, 2000);
    return () => clearInterval(id);
  }, []);

  const act = async (fn: () => Promise<unknown>) => {
    setErr("");
    try {
      await fn();
    } catch (e) {
      setErr(String(e));
    }
  };

  const isServer = mode === "server";
  // A server cares about client machines; a client cares about servers.
  const relevant = peers.filter((p) => (isServer ? p.role === "client" : p.role === "server"));

  return (
    <div className="grid gap-x-16 gap-y-12 lg:grid-cols-2">
      {isServer ? (
        <Section title="Connected machines">
          {clients.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              No machine is connected right now. Nearby machines appear beside this one — or connect
              manually from the Client page on the other machine.
            </p>
          ) : (
            <div className="divide-y divide-border/50">
              {clients.map((c) => (
                <div key={c.name} className="flex items-center justify-between gap-4 py-3">
                  <div className="min-w-0">
                    <div className="text-sm font-medium">{c.name}</div>
                    <div className="font-mono text-[11px] text-muted-foreground/60">
                      {c.addr} · id {shortID(c.id)}
                    </div>
                  </div>
                  <div className="flex shrink-0 items-center gap-1.5">
                    <Button variant="outline" size="sm" onClick={() => void act(() => api().ClientCommand(c.name, "disconnect"))}>
                      Disconnect
                    </Button>
                    <Button variant="outline" size="sm" onClick={() => void act(() => api().ClientCommand(c.name, "restart"))}>
                      Restart
                    </Button>
                  </div>
                </div>
              ))}
            </div>
          )}
          {err && <p className="mt-2 text-xs text-destructive">{err}</p>}
        </Section>
      ) : (
        <Section title="Connection">
          <ConnectionLine status={clientState.status} server={clientState.server || settings?.clientAddr || ""} />
          <p className="text-sm text-muted-foreground">
            {clientState.status === "connected"
              ? "This machine is being controlled from the server."
              : "Start the client from here when a server is ready."}
          </p>
        </Section>
      )}

      <Section title="On this network">
        {relevant.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            No other kvmshare machine is visible on the network yet. They appear here automatically once
            they are running — no addresses to type.
          </p>
        ) : (
          <div className="divide-y divide-border/50">
            {relevant.map((p) => (
              <div key={p.id} className="flex items-center justify-between gap-4 py-3">
                <div className="min-w-0">
                  <div className="text-sm font-medium">{p.name}</div>
                  <div className="font-mono text-[11px] text-muted-foreground/60">
                    {p.addr}:{p.port || DEFAULT_PORT} · id {shortID(p.id)}
                  </div>
                </div>
                <div className="flex shrink-0 items-center gap-1.5">
                  {isServer ? (
                    <>
                      <Button variant="outline" size="sm" onClick={() => void act(() => api().TrustClient(p.id))}>
                        Trust
                      </Button>
                      <Button variant="outline" size="sm" onClick={() => void act(() => api().SendConnectRequest(p.id))}>
                        Connect here
                      </Button>
                    </>
                  ) : (
                    <Button variant="outline" size="sm" onClick={() => void act(() => api().ConnectToServer(`${p.addr}:${p.port || DEFAULT_PORT}`))}>
                      Connect
                    </Button>
                  )}
                </div>
              </div>
            ))}
          </div>
        )}
        {err && <p className="mt-2 text-xs text-destructive">{err}</p>}
      </Section>
    </div>
  );
}