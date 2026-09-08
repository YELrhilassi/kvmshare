import { useEffect, useState } from "react";
import { useApp } from "@/app/AppProvider";
import { api, type ConnectedClient, type Peer } from "@/lib/bridge";
import { DEFAULT_PORT } from "@/lib/constants";
import { Button } from "@/components/ui/button";
import { Section } from "@/components/Section";

// The live half of the dashboard: who is connected to this machine right
// now, and which other kvmshare machines are on the network. Read-only
// views live here (state), not on the config pages.

export default function LiveOverview() {
  const { mode } = useApp();
  const [clients, setClients] = useState<ConnectedClient[]>([]);
  const [peers, setPeers] = useState<Peer[]>([]);
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
      <Section title={isServer ? "Connected machines" : "Controlled by"}>
        {clients.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            {isServer
              ? "No machine is connected right now. Nearby machines appear next to it — or connect manually from the Client page on the other machine."
              : "No server is controlling this machine right now."}
          </p>
        ) : (
          <div className="divide-y divide-border/50">
            {clients.map((c) => (
              <div key={c.name} className="flex items-center justify-between gap-4 py-3">
                <div className="min-w-0">
                  <div className="text-sm font-medium">{c.name}</div>
                  <div className="font-mono text-[11px] text-muted-foreground/60">
                    {c.addr} · id {c.id.slice(0, 8)}…
                  </div>
                </div>
                {isServer && (
                  <div className="flex shrink-0 items-center gap-1.5">
                    <Button variant="outline" size="sm" onClick={() => void act(() => api().ClientCommand(c.name, "disconnect"))}>
                      Disconnect
                    </Button>
                    <Button variant="outline" size="sm" onClick={() => void act(() => api().ClientCommand(c.name, "restart"))}>
                      Restart
                    </Button>
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
        {err && <p className="mt-2 text-xs text-destructive">{err}</p>}
      </Section>

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
                    {p.addr}:{p.port || DEFAULT_PORT} · id {p.id.slice(0, 8)}…
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