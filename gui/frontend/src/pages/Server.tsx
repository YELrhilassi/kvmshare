import { useEffect, useState } from "react";
import { api, type InterfaceInfo, type LayoutConfig, type Paths } from "@/lib/bridge";
import { DEFAULT_PORT } from "@/lib/constants";
import { useLogTail, useRunning } from "@/lib/hooks";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Section, Row } from "@/components/Section";
import { cn } from "@/lib/utils";

export default function ServerPage() {
  const running = useRunning();
  const [paths, setPaths] = useState<Paths | null>(null);
  const [config, setConfig] = useState<LayoutConfig | null>(null);
  const [ifaces, setIfaces] = useState<InterfaceInfo[]>([]);
  const [port, setPort] = useState(String(DEFAULT_PORT));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const { log, viewportRef, onScroll, setStick } = useLogTail(paths?.serverLog);

  useEffect(() => {
    void api()
      .GetPaths()
      .then(setPaths)
      .catch(() => {});
    void api()
      .LoadConfig()
      .then((c) => {
        setConfig(c);
        setPort(String(c.port));
      })
      .catch(() => {});
    void api()
      .ListInterfaces()
      .then(setIfaces)
      .catch(() => {});
  }, []);

  const toggleServer = async () => {
    setBusy(true);
    setError("");
    try {
      if (running.server) await api().ServerStop();
      else await api().ServerStart();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const savePort = async () => {
    setError("");
    const p = parseInt(port, 10);
    if (Number.isNaN(p) || p < 1024 || p > 65535) {
      setError("Port must be between 1024 and 65535");
      return;
    }
    try {
      const c = config ?? { port: p, screens: [] };
      await api().SaveConfig({ ...c, port: p });
      setConfig({ ...c, port: p });
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-2xl px-10 py-16">
        <header className="mb-12 space-y-1">
          <h1 className="text-2xl font-semibold tracking-tight">Server</h1>
          <p className="text-sm text-muted-foreground">Configuration, network and logs.</p>
        </header>

        <div className="space-y-16">
          <Section
            title="Status"
            action={
              <Button
                size="sm"
                variant={running.server ? "outline" : "default"}
                onClick={toggleServer}
                disabled={busy}
              >
                {running.server ? "Stop" : "Start"}
              </Button>
            }
          >
            <div className="flex items-center gap-3">
              <span
                className={cn(
                  "h-2.5 w-2.5 rounded-full",
                  running.server ? "bg-emerald-500" : "bg-muted-foreground/40",
                )}
              />
              <span className="text-lg font-medium">{running.server ? "Running" : "Stopped"}</span>
            </div>
            {error && <p className="text-xs text-destructive">{error}</p>}
          </Section>

          <Section title="Configuration">
            <div className="flex items-end gap-3">
              <div className="space-y-1.5">
                <Label htmlFor="port">Port</Label>
                <Input
                  id="port"
                  type="number"
                  min={1024}
                  max={65535}
                  className="w-36"
                  value={port}
                  onChange={(e) => setPort(e.target.value)}
                />
              </div>
              <Button variant="outline" size="sm" onClick={savePort}>
                Save
              </Button>
            </div>
            <div className="mt-6">
              <Row label="Screens" value={config ? String(config.screens.length) : "…"} />
              <Row label="Config file" value={paths?.configPath ?? "…"} mono />
            </div>
          </Section>

          <Section title="Network">
            <div className="divide-y divide-border/60">
              {ifaces.map((ifc) => (
                <div key={ifc.name} className="flex items-baseline justify-between gap-6 py-3">
                  <span className="font-mono text-sm font-medium">{ifc.name}</span>
                  <span className="text-right">
                    {ifc.addrs.length > 0 ? (
                      ifc.addrs.map((a) => (
                        <span key={a} className="ml-3 font-mono text-sm text-muted-foreground">
                          {a}
                        </span>
                      ))
                    ) : (
                      <span className="text-sm text-muted-foreground/60">no addresses</span>
                    )}
                  </span>
                </div>
              ))}
            </div>
          </Section>

          <Section
            title="Logs"
            action={
              <button
                onClick={() => setStick(true)}
                className="text-xs text-muted-foreground transition-colors hover:text-foreground"
              >
                follow
              </button>
            }
          >
            <ScrollArea className="h-72 rounded-md border border-border/70 bg-muted/30">
              <div
                ref={viewportRef}
                onScroll={onScroll}
                className="h-full overflow-y-auto p-3 font-mono text-xs leading-relaxed whitespace-pre-wrap"
              >
                {log || "— no log output yet —"}
              </div>
            </ScrollArea>
          </Section>
        </div>
      </div>
    </div>
  );
}