import { useCallback, useEffect, useState } from "react";
import { useApp } from "@/app/AppProvider";
import {
  api,
  type ConnectedClient,
  type InterfaceInfo,
  type LayoutConfig,
  type Network,
  type Paths,
  type Peer,
} from "@/lib/bridge";
import { DEFAULT_PORT } from "@/lib/constants";
import { lanAddresses } from "@/lib/net";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import StatusChip from "@/components/StatusChip";
import { Section } from "@/components/Section";

// The server page: what the machine offers (addresses), who is attached
// (connected clients, with per-client control), and who may attach
// (network policy + nearby machines). Configuration stays here — the
// Layout page only arranges screens.
export default function ServerPage() {
  const { running } = useApp();
  const [ips, setIps] = useState<string[]>([]);
  const [config, setConfig] = useState<LayoutConfig | null>(null);
  const [paths, setPaths] = useState<Paths | null>(null);
  const [port, setPort] = useState(String(DEFAULT_PORT));
  const [network, setNetwork] = useState<Network>({ allowlist: true, localOnly: true, trustedIds: [] });
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);
  const [clients, setClients] = useState<ConnectedClient[]>([]);
  const [peers, setPeers] = useState<Peer[]>([]);
  const [machineId, setMachineId] = useState("");
  const [trustInput, setTrustInput] = useState("");
  const [clientErr, setClientErr] = useState("");

  useEffect(() => {
    void api()
      .ListInterfaces()
      .then((ifaces: InterfaceInfo[]) => setIps(lanAddresses(ifaces)))
      .catch(() => {});
    void api()
      .LoadConfig()
      .then((c) => {
        setConfig(c);
        setPort(String(c.port));
        setNetwork(c.network ?? { allowlist: true, localOnly: true, trustedIds: [] });
      })
      .catch(() => {});
    void api()
      .GetPaths()
      .then(setPaths)
      .catch(() => {});
    void api()
      .GetMachineId()
      .then(setMachineId)
      .catch(() => {});
  }, []);

  // Live views: connected clients (from the server's clients.json) and
  // nearby machines (mDNS discovery). Both poll quietly.
  useEffect(() => {
    const tick = () => {
      void api()
        .ListClients()
        .then(setClients)
        .catch(() => {});
      void api()
        .DiscoverPeers()
        .then((ps) => setPeers(ps.filter((p) => p.role === "client")))
        .catch(() => {});
    };
    void tick();
    const id = setInterval(tick, 2000);
    return () => clearInterval(id);
  }, []);

  const dirty = port !== String(config?.port ?? DEFAULT_PORT);

  const savePort = async () => {
    setError("");
    setSaved(false);
    const p = parseInt(port, 10);
    if (Number.isNaN(p) || p < 1024 || p > 65535) {
      setError("Port must be between 1024 and 65535");
      return;
    }
    try {
      const c = config ?? { port: p, screens: [], network };
      await api().SaveConfig({ ...c, port: p });
      setConfig({ ...c, port: p });
      setSaved(true);
      window.setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      setError(String(e));
    }
  };

  const saveNetwork = async () => {
    setError("");
    setSaved(false);
    try {
      const c = config ?? { port: parseInt(port, 10) || DEFAULT_PORT, screens: [], network };
      await api().SaveConfig({ ...c, network });
      setConfig({ ...c, network });
      setSaved(true);
      window.setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      setError(String(e));
    }
  };

  const patchNetwork = (patch: Partial<Network>) => setNetwork((n) => ({ ...n, ...patch }));

  const sendCommand = useCallback(
    async (name: string, action: string) => {
      setClientErr("");
      try {
        await api().ClientCommand(name, action);
      } catch (e) {
        setClientErr(String(e));
      }
    },
    [],
  );

  const trust = async (id: string) => {
    setClientErr("");
    try {
      await api().TrustClient(id);
      setTrustInput("");
    } catch (e) {
      setClientErr(String(e));
    }
  };

  const connectNearby = async (id: string) => {
    setClientErr("");
    try {
      await api().SendConnectRequest(id);
    } catch (e) {
      setClientErr(String(e));
    }
  };

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-3xl px-10 py-16">
        <header className="mb-10 space-y-2">
          <div className="flex items-center gap-3">
            <h1 className="text-2xl font-semibold tracking-tight">Server</h1>
            <StatusChip active={running.server} activeLabel="Sharing" idleLabel="Stopped" />
          </div>
          <p className="text-sm text-muted-foreground">
            This machine owns the shared keyboard and mouse. Machine id:{" "}
            <span className="font-mono text-xs">{machineId || "…"}</span>
          </p>
        </header>

        <Section title="How other machines connect">
          <div className="space-y-1.5">
            {ips.map((ip) => (
              <div key={ip} className="font-mono text-2xl tracking-tight">
                {ip}:{port}
              </div>
            ))}
            {ips.length === 0 && (
              <div className="font-mono text-2xl text-muted-foreground">no network address found</div>
            )}
          </div>

          <div className="flex items-center gap-2 border-t border-border/60 pt-4">
            <Label htmlFor="port" className="text-xs text-muted-foreground">
              Port
            </Label>
            <Input
              id="port"
              type="number"
              min={1024}
              max={65535}
              className="w-24"
              value={port}
              onChange={(e) => setPort(e.target.value)}
            />
            <Button variant={dirty ? "default" : "outline"} size="sm" onClick={savePort} disabled={!dirty}>
              Save
            </Button>
          </div>
        </Section>

        <Section title="Connected clients" className="mt-12">
          {clients.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              No clients connected. Tell a nearby machine to connect to one of the addresses above.
            </p>
          ) : (
            <div className="divide-y divide-border/50">
              {clients.map((c) => (
                <div key={c.name} className="flex items-center justify-between gap-4 py-3">
                  <div className="min-w-0">
                    <div className="flex items-baseline gap-2">
                      <span className="text-sm font-medium">{c.name}</span>
                      <span className="font-mono text-[11px] text-muted-foreground/70">{c.addr}</span>
                    </div>
                    <div className="font-mono text-[11px] text-muted-foreground/50">
                      id {c.id.slice(0, 8)}…
                    </div>
                  </div>
                  <div className="flex shrink-0 items-center gap-1.5">
                    <Button variant="outline" size="sm" onClick={() => void sendCommand(c.name, "disconnect")}>
                      Disconnect
                    </Button>
                    <Button variant="outline" size="sm" onClick={() => void sendCommand(c.name, "reconnect")}>
                      Reconnect
                    </Button>
                    <Button variant="outline" size="sm" onClick={() => void sendCommand(c.name, "restart")}>
                      Restart
                    </Button>
                  </div>
                </div>
              ))}
            </div>
          )}
          {clientErr && <p className="text-xs text-destructive">{clientErr}</p>}
        </Section>

        <Section title="Who may connect" className="mt-12">
          <div className="space-y-4">
            <div className="flex items-center justify-between">
              <div>
                <div className="text-sm">Only named clients</div>
                <p className="text-xs text-muted-foreground">
                  Accept only clients whose exact name is in the layout, plus trusted machine ids.
                </p>
              </div>
              <Switch checked={network.allowlist} onCheckedChange={(v) => patchNetwork({ allowlist: v })} />
            </div>

            <div className="flex items-center justify-between">
              <div>
                <div className="text-sm">Local network only</div>
                <p className="text-xs text-muted-foreground">
                  Refuse connections from outside the local network.
                </p>
              </div>
              <Switch checked={network.localOnly} onCheckedChange={(v) => patchNetwork({ localOnly: v })} />
            </div>

            <div className="space-y-2">
              <Label htmlFor="trusted" className="text-xs text-muted-foreground">
                Trusted machine ids — allowed to connect even before they appear in the layout
              </Label>
              <div className="flex flex-wrap gap-1.5">
                {network.trustedIds.map((id) => (
                  <span
                    key={id}
                    className="inline-flex items-center gap-1 rounded-full border border-border/70 px-2 py-0.5 font-mono text-[11px]"
                  >
                    {id.slice(0, 8)}…
                    <button
                      className="text-muted-foreground/60 hover:text-destructive"
                      onClick={() => patchNetwork({ trustedIds: network.trustedIds.filter((t) => t !== id) })}
                      aria-label={`remove ${id}`}
                    >
                      ×
                    </button>
                  </span>
                ))}
              </div>
              <div className="flex items-center gap-2">
                <Input
                  id="trusted"
                  className="w-64 font-mono"
                  placeholder="paste a machine id"
                  value={trustInput}
                  onChange={(e) => setTrustInput(e.target.value)}
                />
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => {
                    if (trustInput.trim()) {
                      patchNetwork({ trustedIds: [...network.trustedIds, trustInput.trim()] });
                      setTrustInput("");
                    }
                  }}
                >
                  Add
                </Button>
              </div>
            </div>

            <div className="flex items-center gap-2">
              <Button variant={dirty || saved ? "default" : "outline"} size="sm" onClick={saveNetwork}>
                Save network settings
              </Button>
              {saved && <span className="text-xs text-emerald-600">saved</span>}
              {error && <span className="text-xs text-destructive">{error}</span>}
            </div>
          </div>
        </Section>

        <Section title="Nearby machines" className="mt-12">
          {peers.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              No other kvmshare machines found on the network yet.
            </p>
          ) : (
            <div className="divide-y divide-border/50">
              {peers.map((p) => (
                <div key={p.id} className="flex items-center justify-between gap-4 py-3">
                  <div className="min-w-0">
                    <div className="text-sm font-medium">{p.name}</div>
                    <div className="font-mono text-[11px] text-muted-foreground/50">
                      {p.addr} · id {p.id.slice(0, 8)}…
                    </div>
                  </div>
                  <div className="flex shrink-0 items-center gap-1.5">
                    <Button variant="outline" size="sm" onClick={() => void trust(p.id)}>
                      Trust
                    </Button>
                    <Button variant="outline" size="sm" onClick={() => void connectNearby(p.id)}>
                      Ask to connect
                    </Button>
                  </div>
                </div>
              ))}
            </div>
          )}
          <p className="text-xs text-muted-foreground/60">
            "Ask to connect" tells that machine to connect here; it accepts when you have trusted each
            other. Manual addresses above always work as a fallback.
          </p>
        </Section>

        <footer className="mt-14 border-t border-border/60 pt-4 text-[11px] text-muted-foreground/50">
          Config: <span className="font-mono">{paths?.configPath ?? "…"}</span> · start/stop from Home
        </footer>
      </div>
    </div>
  );
}