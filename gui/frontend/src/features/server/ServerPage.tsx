import { useEffect, useState } from "react";
import { useApp } from "@/app/AppProvider";
import { api, type InterfaceInfo, type LayoutConfig, type Paths } from "@/lib/bridge";
import { DEFAULT_PORT } from "@/lib/constants";
import { lanAddresses } from "@/lib/net";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import StatusChip from "@/components/StatusChip";
import { Section } from "@/components/Section";

// The server page answers one question: "what do I tell the other
// machine?" — the addresses, big. The port sits right below it inline.
// No interface dump, no config trivia; the config path is one muted
// footer line for when something goes wrong.
export default function ServerPage() {
  const { running } = useApp();
  const [ips, setIps] = useState<string[]>([]);
  const [config, setConfig] = useState<LayoutConfig | null>(null);
  const [paths, setPaths] = useState<Paths | null>(null);
  const [port, setPort] = useState(String(DEFAULT_PORT));
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);

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
      })
      .catch(() => {});
    void api()
      .GetPaths()
      .then(setPaths)
      .catch(() => {});
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
      const c = config ?? { port: p, screens: [] };
      await api().SaveConfig({ ...c, port: p });
      setConfig({ ...c, port: p });
      setSaved(true);
      window.setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-2xl px-10 py-16">
        <header className="mb-10 space-y-2">
          <div className="flex items-center gap-3">
            <h1 className="text-2xl font-semibold tracking-tight">Server</h1>
            <StatusChip active={running.server} activeLabel="Sharing" idleLabel="Stopped" />
          </div>
          <p className="text-sm text-muted-foreground">Other machines control this one from here.</p>
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
            {saved && <span className="text-xs text-emerald-600">saved</span>}
            {error && <span className="text-xs text-destructive">{error}</span>}
          </div>
        </Section>

        <footer className="mt-14 border-t border-border/60 pt-4 text-[11px] text-muted-foreground/50">
          Config: <span className="font-mono">{paths?.configPath ?? "…"}</span> · start/stop from Home
        </footer>
      </div>
    </div>
  );
}