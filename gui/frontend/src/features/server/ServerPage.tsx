import { useEffect, useState } from "react";
import { api, type LayoutConfig, type Network, type Paths } from "@/lib/bridge";
import { DEFAULT_PORT } from "@/lib/constants";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Section } from "@/components/Section";

// Server settings: how this machine shares its keyboard and mouse, and
// who is allowed to connect. Live state (who is connected, what is on
// the network) is on Home — this page only configures.
export default function ServerPage() {
  const [config, setConfig] = useState<LayoutConfig | null>(null);
  const [paths, setPaths] = useState<Paths | null>(null);
  const [port, setPort] = useState(String(DEFAULT_PORT));
  const [network, setNetwork] = useState<Network>({ allowlist: true, localOnly: true, trustedIds: [] });
  const [machineId, setMachineId] = useState("");
  const [trustInput, setTrustInput] = useState("");
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);

  useEffect(() => {
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

  const patchNetwork = (patch: Partial<Network>) => setNetwork((n) => ({ ...n, ...patch }));

  const save = async () => {
    setError("");
    setSaved(false);
    const p = parseInt(port, 10);
    if (Number.isNaN(p) || p < 1024 || p > 65535) {
      setError("Port must be between 1024 and 65535");
      return;
    }
    const c = config ?? { port: p, screens: [], network };
    try {
      await api().SaveConfig({ ...c, port: p, network });
      setConfig({ ...c, port: p, network });
      setSaved(true);
      window.setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      setError(String(e));
    }
  };

  const dirty =
    port !== String(config?.port ?? DEFAULT_PORT) ||
    network.allowlist !== (config?.network?.allowlist ?? true) ||
    network.localOnly !== (config?.network?.localOnly ?? true) ||
    JSON.stringify(network.trustedIds) !== JSON.stringify(config?.network?.trustedIds ?? []);

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-3xl px-10 py-16">
        <header className="mb-10 space-y-2">
          <h1 className="text-2xl font-semibold tracking-tight">Server settings</h1>
          <p className="text-sm text-muted-foreground">
            How this machine shares its keyboard and mouse, and who may connect to it.
          </p>
        </header>

        <Section title="Port">
          <div className="flex items-center gap-2">
            <Input
              type="number"
              min={1024}
              max={65535}
              className="w-28"
              value={port}
              onChange={(e) => setPort(e.target.value)}
              aria-label="Server port"
            />
            <span className="text-sm text-muted-foreground">
              Other machines connect to this port (the default works for most setups).
            </span>
          </div>
        </Section>

        <Section title="Who may connect" className="mt-12">
          <div className="space-y-4">
            <div className="flex items-center justify-between gap-6">
              <div>
                <div className="text-sm">Only machines in the layout</div>
                <p className="text-xs text-muted-foreground">
                  Accept only machines whose name appears in the layout, plus the trusted ids below.
                </p>
              </div>
              <Switch checked={network.allowlist} onCheckedChange={(v) => patchNetwork({ allowlist: v })} />
            </div>

            <div className="flex items-center justify-between gap-6">
              <div>
                <div className="text-sm">Same network only</div>
                <p className="text-xs text-muted-foreground">Refuse connections from outside this network.</p>
              </div>
              <Switch checked={network.localOnly} onCheckedChange={(v) => patchNetwork({ localOnly: v })} />
            </div>
          </div>
        </Section>

        <Section title="Trusted machines" className="mt-12">
          <p className="text-sm text-muted-foreground">
            A trusted machine may connect even before it appears in the layout. Its id is shown on its own
            Home page.
          </p>
          <div className="flex flex-wrap gap-1.5">
            {network.trustedIds.length === 0 && (
              <span className="text-xs text-muted-foreground/60">No trusted machines yet.</span>
            )}
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
              className="w-64 font-mono"
              placeholder="paste a machine id"
              value={trustInput}
              onChange={(e) => setTrustInput(e.target.value)}
              aria-label="Machine id to trust"
            />
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                const id = trustInput.trim();
                if (id) {
                  patchNetwork({ trustedIds: [...network.trustedIds, id] });
                  setTrustInput("");
                }
              }}
            >
              Add
            </Button>
          </div>
        </Section>

        <div className="mt-12 flex items-center gap-3">
          <Button onClick={save} disabled={!dirty}>
            {dirty ? "Save changes" : "Saved"}
          </Button>
          {saved && <span className="text-xs text-emerald-600">saved</span>}
          {error && <span className="text-xs text-destructive">{error}</span>}
        </div>

        <footer className="mt-14 space-y-1 border-t border-border/60 pt-4 text-[11px] text-muted-foreground/50">
          <div>
            This machine's id: <span className="font-mono">{machineId || "…"}</span>
          </div>
          <div>
            Config: <span className="font-mono">{paths?.configPath ?? "…"}</span>
          </div>
        </footer>
      </div>
    </div>
  );
}