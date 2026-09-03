import { useEffect, useState } from "react";
import { api, type Mode, type Settings } from "@/lib/bridge";
import { DEFAULT_PORT } from "@/lib/constants";
import type { RunningStatus } from "@/lib/hooks";
import { pagesFor, type Page } from "@/lib/nav";
import { Button } from "@/components/ui/button";
import { Section } from "@/components/Section";
import { cn } from "@/lib/utils";

interface Props {
  mode: Mode;
  onModeChange: (m: Mode) => Promise<void>;
  onNavigate: (p: Page) => void;
  running: RunningStatus;
}

export default function HomePage({ mode, onModeChange, onNavigate, running }: Props) {
  const [ips, setIps] = useState<string[]>([]);
  const [port, setPort] = useState(DEFAULT_PORT);
  const [screens, setScreens] = useState(0);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    void api()
      .GetSettings()
      .then(setSettings)
      .catch(() => {});
    void api()
      .LoadConfig()
      .then((c) => {
        setPort(c.port);
        setScreens(c.screens.length);
      })
      .catch(() => {});
    void api()
      .ListInterfaces()
      .then((ifaces) => {
        const v4: string[] = [];
        for (const ifc of ifaces) {
          for (const addr of ifc.addrs) {
            if (
              /^\d+\.\d+\.\d+\.\d+$/.test(addr) &&
              !addr.startsWith("127.") &&
              !addr.startsWith("169.254.")
            ) {
              v4.push(addr);
            }
          }
        }
        // Private ranges first — those are the usual LAN addresses.
        const score = (ip: string) =>
          ip.startsWith("192.168.") ? 0 : ip.startsWith("10.") ? 1 : ip.startsWith("172.") ? 2 : 3;
        v4.sort((a, b) => score(a) - score(b));
        setIps(v4.slice(0, 3));
      })
      .catch(() => {});
  }, []);

  const active = mode === "server" ? running.server : running.client;

  const toggle = async () => {
    setBusy(true);
    setError("");
    try {
      if (active) await api().StopActive();
      else await api().StartActive();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const quickLinks = pagesFor(mode).filter((p) => p !== "home");

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-2xl px-10 py-16">
        <div className="space-y-20">
          <Section title="This machine">
            <div className="inline-flex rounded-lg border bg-muted/40 p-1">
              {(["server", "client"] as Mode[]).map((m) => (
                <button
                  key={m}
                  onClick={() => void onModeChange(m)}
                  className={cn(
                    "rounded-md px-7 py-2 text-sm font-medium transition-colors",
                    mode === m
                      ? "bg-background shadow-sm"
                      : "text-muted-foreground hover:text-foreground",
                  )}
                >
                  {m === "server" ? "Server" : "Client"}
                </button>
              ))}
            </div>
            <p className="max-w-md text-sm text-muted-foreground">
              {mode === "server"
                ? "Share this machine's keyboard and mouse with other machines."
                : "Let another machine control this one."}
            </p>
            <p className="text-xs text-muted-foreground/70">
              {active
                ? "Switching role stops the running process."
                : "This machine runs as one role at a time."}
            </p>
          </Section>

          <Section title={mode === "server" ? "Status" : "Connection"}>
            <div className="flex items-center gap-3">
              <span
                className={cn(
                  "h-2.5 w-2.5 rounded-full",
                  active ? "bg-emerald-500" : "bg-muted-foreground/40",
                )}
              />
              <span className="text-2xl font-semibold tracking-tight">
                {active ? "Running" : "Stopped"}
              </span>
            </div>

            {mode === "server" ? (
              <div className="space-y-1">
                <div className="text-xs tracking-wider text-muted-foreground uppercase">
                  Clients connect to
                </div>
                {ips.map((ip) => (
                  <div key={ip} className="font-mono text-lg">
                    {ip}:{port}
                  </div>
                ))}
                {ips.length === 0 && (
                  <div className="font-mono text-lg text-muted-foreground">
                    no network address found
                  </div>
                )}
              </div>
            ) : (
              settings && (
                <div className="text-lg">
                  <span className="font-mono">{settings.clientAddr || "—"}</span>
                  <span className="mx-2 text-muted-foreground">as</span>
                  <span className="font-mono">{settings.clientName || "—"}</span>
                </div>
              )
            )}

            <div className="space-y-2">
              <Button onClick={toggle} disabled={busy} className="w-44">
                {active ? `Stop ${mode}` : `Start ${mode}`}
              </Button>
              {error && <p className="text-xs text-destructive">{error}</p>}
            </div>
          </Section>

          <Section title="Quick access">
            <div className="flex flex-wrap gap-x-10 gap-y-3">
              {quickLinks.map((p) => (
                <button
                  key={p}
                  onClick={() => onNavigate(p)}
                  className="text-sm text-muted-foreground transition-colors hover:text-foreground"
                >
                  {p === "layout"
                    ? `Layout · ${screens} screens`
                    : p === "server"
                      ? "Server settings"
                      : "Client settings"}
                </button>
              ))}
            </div>
          </Section>

          <UpdateLine />
        </div>
      </div>
    </div>
  );
}

function UpdateLine() {
  const [version, setVersion] = useState("");
  const [state, setState] = useState<"idle" | "checking" | "uptodate" | "available" | "applying" | "error">("idle");
  const [newVersion, setNewVersion] = useState("");
  const [error, setError] = useState("");

  useEffect(() => {
    void api()
      .GetVersion()
      .then(setVersion)
      .catch(() => {});
  }, []);

  const check = async () => {
    setState("checking");
    setError("");
    try {
      const info = await api().CheckForUpdate();
      if (info.error) {
        setState("error");
        setError(info.error);
      } else if (info.available) {
        setNewVersion(info.version);
        setState("available");
      } else {
        setState("uptodate");
      }
    } catch (e) {
      setState("error");
      setError(String(e));
    }
  };

  const apply = async () => {
    setState("applying");
    const res = await api().ApplyUpdate();
    if (res.error) {
      setState("error");
      setError(res.error);
    }
    // Success: the backend restarts the app shortly; leave the line as is.
  };

  return (
    <div className="flex items-center gap-3 border-t pt-5 text-xs text-muted-foreground/70">
      <span className="font-mono">{version || "kvmshare"}</span>
      <span className="h-3 w-px bg-border" />
      {state === "idle" && (
        <button onClick={() => void check()} className="transition-colors hover:text-foreground">
          Check for updates
        </button>
      )}
      {state === "checking" && <span>Checking…</span>}
      {state === "uptodate" && <span>Up to date</span>}
      {state === "available" && (
        <button onClick={() => void apply()} className="text-foreground transition-colors hover:opacity-70">
          Update {newVersion} available — install &amp; restart
        </button>
      )}
      {state === "applying" && <span>Installing — restarting…</span>}
      {state === "error" && (
        <button onClick={() => void check()} className="text-destructive transition-colors hover:opacity-70" title={error}>
          Update check failed — retry
        </button>
      )}
    </div>
  );
}