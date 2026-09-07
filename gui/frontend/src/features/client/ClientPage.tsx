import { useEffect, useState } from "react";
import { useApp } from "@/app/AppProvider";
import { api, type Peer, type Settings } from "@/lib/bridge";
import { DEFAULT_PORT } from "@/lib/constants";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import StatusChip from "@/components/StatusChip";
import { Section } from "@/components/Section";

// The client page: which machine controls this one, found by discovery
// (click a nearby server) or by address (manual fallback). Auto-connect
// and the trusted-servers list make the connection automatic once set up.
export default function ClientPage() {
  const { running } = useApp();
  const [settings, setSettings] = useState<Settings | null>(null);
  const [addr, setAddr] = useState("");
  const [name, setName] = useState("");
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);
  const [peers, setPeers] = useState<Peer[]>([]);
  const [trusted, setTrusted] = useState<string[]>([]);
  const [trustInput, setTrustInput] = useState("");
  const [actionErr, setActionErr] = useState("");

  useEffect(() => {
    void api()
      .GetSettings()
      .then((s) => {
        setSettings(s);
        setAddr(s.clientAddr);
        setName(s.clientName);
        setTrusted(s.trustedServers ?? []);
      })
      .catch(() => {});
  }, []);

  // Nearby servers (mDNS), refreshed quietly.
  useEffect(() => {
    const tick = () => {
      void api()
        .DiscoverPeers()
        .then((ps) => setPeers(ps.filter((p) => p.role === "server")))
        .catch(() => {});
    };
    void tick();
    const id = setInterval(tick, 2000);
    return () => clearInterval(id);
  }, []);

  const dirty = settings !== null && (addr.trim() !== settings.clientAddr || name.trim() !== settings.clientName);

  const save = async () => {
    setError("");
    setSaved(false);
    if (!settings) return;
    const next = { ...settings, clientAddr: addr.trim(), clientName: name.trim() };
    try {
      await api().SetSettings(next);
      setSettings(next);
      setSaved(true);
      window.setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      setError(String(e));
    }
  };

  const connectTo = async (peer: Peer) => {
    setActionErr("");
    try {
      await api().ConnectToServer(`${peer.addr}:${peer.port || DEFAULT_PORT}`);
    } catch (e) {
      setActionErr(String(e));
    }
  };

  const saveTrust = async (ids: string[]) => {
    setActionErr("");
    if (!settings) return;
    try {
      const next = { ...settings, trustedServers: ids };
      await api().SetSettings(next);
      setSettings(next);
      setTrusted(ids);
    } catch (e) {
      setActionErr(String(e));
    }
  };

  const toggleAuto = async (v: boolean) => {
    setActionErr("");
    if (!settings) return;
    const next = { ...settings, autoConnect: v };
    try {
      await api().SetSettings(next);
      setSettings(next);
    } catch (e) {
      setActionErr(String(e));
    }
  };

  const togglePairing = async (v: boolean) => {
    setActionErr("");
    if (!settings) return;
    const next = { ...settings, acceptPairing: v };
    try {
      await api().SetSettings(next);
      setSettings(next);
    } catch (e) {
      setActionErr(String(e));
    }
  };

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-3xl px-10 py-16">
        <header className="mb-10 space-y-2">
          <div className="flex items-center gap-3">
            <h1 className="text-2xl font-semibold tracking-tight">Client</h1>
            <StatusChip active={running.client} activeLabel="Connected" idleLabel="Not connected" />
          </div>
          <p className="text-sm text-muted-foreground">Controlled from another machine.</p>
        </header>

        <Section title="Nearby servers">
          {peers.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              No kvmshare servers found on the network yet. They appear here automatically — or use the
              address below.
            </p>
          ) : (
            <div className="divide-y divide-border/50">
              {peers.map((p) => (
                <div key={p.id} className="flex items-center justify-between gap-4 py-3">
                  <div className="min-w-0">
                    <div className="text-sm font-medium">{p.name}</div>
                    <div className="font-mono text-[11px] text-muted-foreground/50">
                      {p.addr}:{p.port || DEFAULT_PORT}
                    </div>
                  </div>
                  <Button variant="outline" size="sm" onClick={() => void connectTo(p)}>
                    Connect
                  </Button>
                </div>
              ))}
            </div>
          )}
          {actionErr && <p className="text-xs text-destructive">{actionErr}</p>}
        </Section>

        <Section title="Connect to" className="mt-12">
          <div className="grid max-w-lg grid-cols-1 gap-4 sm:grid-cols-2">
            <div className="space-y-1.5">
              <Label htmlFor="addr">Server address</Label>
              <Input
                id="addr"
                placeholder={`192.0.2.1:${DEFAULT_PORT}`}
                value={addr}
                onChange={(e) => setAddr(e.target.value)}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="name">Screen name</Label>
              <Input
                id="name"
                placeholder="hp"
                value={name}
                onChange={(e) => setName(e.target.value)}
              />
            </div>
          </div>

          <div className="flex items-center gap-2">
            <Button variant={dirty ? "default" : "outline"} size="sm" onClick={save} disabled={!dirty}>
              Save
            </Button>
            {saved && <span className="text-xs text-emerald-600">saved</span>}
            {error && <span className="text-xs text-destructive">{error}</span>}
          </div>

          {(addr.trim() || name.trim()) && (
            <p className="text-sm text-muted-foreground">
              This machine appears as{" "}
              <span className="font-medium text-foreground/80">{name.trim() || "—"}</span> on{" "}
              <span className="font-mono">{addr.trim() || "—"}</span>.
            </p>
          )}
        </Section>

        <Section title="Automatic connection" className="mt-12">
          <div className="flex items-center justify-between">
            <div>
              <div className="text-sm">Auto-connect</div>
              <p className="text-xs text-muted-foreground">
                Connect automatically when a trusted server appears on the network.
              </p>
            </div>
            <Switch
              checked={settings?.autoConnect ?? false}
              onCheckedChange={(v) => void toggleAuto(v)}
            />
          </div>

          <div className="flex items-center justify-between">
            <div>
              <div className="text-sm">Accept pairing requests</div>
              <p className="text-xs text-muted-foreground">
                Allow any local server to ask this machine to connect (convenient, less strict).
              </p>
            </div>
            <Switch
              checked={settings?.acceptPairing ?? false}
              onCheckedChange={(v) => void togglePairing(v)}
            />
          </div>

          <div className="space-y-2">
            <Label htmlFor="trusted-servers" className="text-xs text-muted-foreground">
              Trusted servers — their connection requests are always accepted
            </Label>
            <div className="flex flex-wrap gap-1.5">
              {trusted.map((id) => (
                <span
                  key={id}
                  className="inline-flex items-center gap-1 rounded-full border border-border/70 px-2 py-0.5 font-mono text-[11px]"
                >
                  {id.slice(0, 8)}…
                  <button
                    className="text-muted-foreground/60 hover:text-destructive"
                    onClick={() => void saveTrust(trusted.filter((t) => t !== id))}
                    aria-label={`remove ${id}`}
                  >
                    ×
                  </button>
                </span>
              ))}
            </div>
            <div className="flex items-center gap-2">
              <Input
                id="trusted-servers"
                className="w-64 font-mono"
                placeholder="paste a server machine id"
                value={trustInput}
                onChange={(e) => setTrustInput(e.target.value)}
              />
              <Button
                variant="outline"
                size="sm"
                onClick={() => {
                  if (trustInput.trim()) {
                    void saveTrust([...trusted, trustInput.trim()]);
                    setTrustInput("");
                  }
                }}
              >
                Add
              </Button>
            </div>
          </div>
        </Section>

        <footer className="mt-14 border-t border-border/60 pt-4 text-[11px] text-muted-foreground/50">
          connect from Home
        </footer>
      </div>
    </div>
  );
}