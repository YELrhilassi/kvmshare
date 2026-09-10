import { useEffect, useState } from "react";
import { api, type Settings } from "@/lib/bridge";
import { DEFAULT_PORT } from "@/lib/constants";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Section } from "@/components/Section";
import { shortID } from "@/lib/utils";

// Client settings: which machine controls this one and how it connects.
// Live state (nearby servers, the current connection) is on Home — this
// page only configures.
export default function ClientPage() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [addr, setAddr] = useState("");
  const [name, setName] = useState("");
  const [trustInput, setTrustInput] = useState("");
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

  const save = async () => {
    setError("");
    setSaved(false);
    if (!settings) return;
    try {
      await api().SetSettings({ ...settings, clientAddr: addr.trim(), clientName: name.trim() });
      setSettings({ ...settings, clientAddr: addr.trim(), clientName: name.trim() });
      setSaved(true);
      window.setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      setError(String(e));
    }
  };

  const saveTrust = async (ids: string[]) => {
    if (!settings) return;
    try {
      await api().SetSettings({ ...settings, trustedServers: ids });
      setSettings({ ...settings, trustedServers: ids });
    } catch (e) {
      setError(String(e));
    }
  };

  const flip = async (p: Partial<Settings>) => {
    if (!settings) return;
    try {
      await api().SetSettings({ ...settings, ...p });
      // Re-read rather than patching the local copy: the backend owns
      // fields this form never edits (revoked ids, the auto-connect
      // pause) and re-arms the pause when auto-connect is turned on.
      setSettings(await api().GetSettings());
    } catch (e) {
      setError(String(e));
    }
  };

  const dirty =
    settings !== null &&
    (addr.trim() !== settings.clientAddr || name.trim() !== settings.clientName);

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-3xl px-10 py-16">
        <header className="mb-10 space-y-2">
          <h1 className="text-2xl font-semibold tracking-tight">Client settings</h1>
          <p className="text-sm text-muted-foreground">
            How this machine connects to a server, and when it is allowed to.
          </p>
        </header>

        <Section title="Server to connect to">
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
              <Label htmlFor="name">Name on the server</Label>
              <Input
                id="name"
                placeholder="machine name"
                value={name}
                onChange={(e) => setName(e.target.value)}
              />
            </div>
          </div>
          <p className="text-xs text-muted-foreground">
            The server sees this machine under this name. It must match the layout on the server, or be
            trusted by it.
          </p>
          <div className="flex items-center gap-3">
            <Button onClick={save} disabled={!dirty}>
              {dirty ? "Save" : "Saved"}
            </Button>
            {saved && <span className="text-xs text-emerald-600">saved</span>}
            {error && <span className="text-xs text-destructive">{error}</span>}
          </div>
        </Section>

        <Section title="Automatic connection" className="mt-12">
          <div className="flex items-center justify-between gap-6">
            <div>
              <div className="text-sm">Connect automatically</div>
              <p className="text-xs text-muted-foreground">
                Automatically connect to the server above whenever it is on this network.
              </p>
              {settings?.autoConnect && settings.autoConnectPaused && (
                <p className="mt-1 text-xs text-amber-600">
                  Held off after your last stop. Press Connect to resume automatic connection.
                </p>
              )}
            </div>
            <Switch checked={settings?.autoConnect ?? false} onCheckedChange={(v) => void flip({ autoConnect: v })} />
          </div>

          <div className="flex items-center justify-between gap-6">
            <div>
              <div className="text-sm">Accept connection requests</div>
              <p className="text-xs text-muted-foreground">
                Allow nearby servers to request a connection. Handy for first-time setup.
              </p>
            </div>
            <Switch checked={settings?.acceptPairing ?? false} onCheckedChange={(v) => void flip({ acceptPairing: v })} />
          </div>

          <div className="space-y-2">
            <Label htmlFor="trusted-servers" className="text-xs text-muted-foreground">
              Trusted servers — their connection requests are always accepted
            </Label>
            <div className="flex flex-wrap gap-1.5">
              {(settings?.trustedServers ?? []).length === 0 && (
                <span className="text-xs text-muted-foreground/60">No trusted servers yet.</span>
              )}
              {(settings?.trustedServers ?? []).map((id) => (
                <span
                  key={id}
                  className="inline-flex items-center gap-1 rounded-full border border-border/70 px-2 py-0.5 font-mono text-[11px]"
                >
                  {shortID(id)}
                  <button
                    className="text-muted-foreground/60 hover:text-destructive"
                    onClick={() => void saveTrust((settings?.trustedServers ?? []).filter((t) => t !== id))}
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
                placeholder="paste a server machine id (short form works)"
                value={trustInput}
                onChange={(e) => setTrustInput(e.target.value)}
              />
              <Button
                variant="outline"
                size="sm"
                onClick={() => {
                  const id = trustInput.trim();
                  if (id) {
                    void saveTrust([...(settings?.trustedServers ?? []), id]);
                    setTrustInput("");
                  }
                }}
              >
                Add
              </Button>
            </div>
          </div>
        </Section>
      </div>
    </div>
  );
}