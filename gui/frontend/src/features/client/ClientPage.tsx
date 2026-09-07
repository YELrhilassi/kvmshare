import { useEffect, useState } from "react";
import { useApp } from "@/app/AppProvider";
import { api, type Settings } from "@/lib/bridge";
import { DEFAULT_PORT } from "@/lib/constants";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import StatusChip from "@/components/StatusChip";
import { Section } from "@/components/Section";

// The client page answers one question: "which machine do I let control
// me?" — the server address and this machine's name. A live preview
// shows the result before saving.
export default function ClientPage() {
  const { running } = useApp();
  const [settings, setSettings] = useState<Settings | null>(null);
  const [addr, setAddr] = useState("");
  const [name, setName] = useState("");
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    void api()
      .GetSettings()
      .then((s) => {
        setSettings(s);
        setAddr(s.clientAddr);
        setName(s.clientName);
      })
      .catch(() => {});
  }, []);

  const dirty =
    settings !== null && (addr.trim() !== settings.clientAddr || name.trim() !== settings.clientName);

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

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-2xl px-10 py-16">
        <header className="mb-10 space-y-2">
          <div className="flex items-center gap-3">
            <h1 className="text-2xl font-semibold tracking-tight">Client</h1>
            <StatusChip active={running.client} activeLabel="Connected" idleLabel="Not connected" />
          </div>
          <p className="text-sm text-muted-foreground">Controlled from another machine.</p>
        </header>

        <Section title="Connect to">
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

        <footer className="mt-14 border-t border-border/60 pt-4 text-[11px] text-muted-foreground/50">
          connect from Home
        </footer>
      </div>
    </div>
  );
}