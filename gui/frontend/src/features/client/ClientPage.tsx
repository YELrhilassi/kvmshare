import { useEffect, useState } from "react";
import { useApp } from "@/app/AppProvider";
import { api, type Settings } from "@/lib/bridge";
import { DEFAULT_PORT } from "@/lib/constants";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Section } from "@/components/Section";
import { cn } from "@/lib/utils";

// Connection settings for the controlled machine. Start/stop lives on
// Home — this page only shows a read-only status.
export default function ClientPage() {
  const { running } = useApp();
  const [settings, setSettings] = useState<Settings | null>(null);
  const [addr, setAddr] = useState("");
  const [name, setName] = useState("");
  const [error, setError] = useState("");

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

  const saveSettings = async () => {
    setError("");
    if (!settings) return;
    const next = { ...settings, clientAddr: addr.trim(), clientName: name.trim() };
    try {
      await api().SetSettings(next);
      setSettings(next);
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-2xl px-10 py-16">
        <header className="mb-12 space-y-1">
          <h1 className="text-2xl font-semibold tracking-tight">Client</h1>
          <p className="text-sm text-muted-foreground">
            Settings for the machine controlled from another computer.
          </p>
        </header>

        <div className="space-y-14">
          <Section title="Connection">
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
            <div className="flex items-center gap-3">
              <Button variant="outline" size="sm" onClick={saveSettings}>
                Save
              </Button>
              {error && <p className="text-xs text-destructive">{error}</p>}
            </div>
          </Section>

          <Section title="Status">
            <div className="flex items-center gap-3">
              <span
                className={cn(
                  "h-2.5 w-2.5 rounded-full",
                  running.client ? "bg-emerald-500" : "bg-muted-foreground/40",
                )}
              />
              <span className="text-lg font-medium">{running.client ? "Connected" : "Not connected"}</span>
              {!running.client && (
                <span className="text-xs text-muted-foreground">connect from Home</span>
              )}
            </div>
            {settings && (
              <p className="text-sm text-muted-foreground">
                Connects to <span className="font-mono">{settings.clientAddr}</span> as{" "}
                <span className="font-mono">{settings.clientName}</span>
              </p>
            )}
          </Section>
        </div>
      </div>
    </div>
  );
}