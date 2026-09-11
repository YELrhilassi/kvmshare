import { useEffect, useState } from "react";
import { useApp } from "@/app/AppProvider";
import { api, type Settings } from "@/lib/bridge";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Section } from "@/components/Section";
import { PageSkeleton } from "@/components/PageSkeleton";
import { cn } from "@/lib/utils";

// General settings: the machine-level choices that are not tied to one
// role — how the GUI starts, what it does at boot, and the network
// conveniences that span both roles. Role-specific configuration stays
// on the Server and Client pages; this page only speaks "this machine".

type StartRole = "" | "server" | "client";

const START_ROLES: { value: StartRole; label: string; hint: string }[] = [
  { value: "", label: "Nothing", hint: "The window opens and waits for you" },
  {
    value: "server",
    label: "Server",
    hint: "Share this machine's keyboard and mouse as soon as it logs in",
  },
  {
    value: "client",
    label: "Client",
    hint: "Connect to the last server as soon as it logs in",
  },
];

function Toggle({
  checked,
  onChange,
  title,
  description,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  title: string;
  description: string;
}) {
  return (
    <div className="flex items-center justify-between gap-6 py-3">
      <div>
        <div className="text-sm">{title}</div>
        <p className="text-xs text-muted-foreground">{description}</p>
      </div>
      <Switch checked={checked} onCheckedChange={onChange} />
    </div>
  );
}

export default function SettingsPage() {
  const { refresh } = useApp();
  const [settings, setSettings] = useState<Settings | null>(null);
  const [autostart, setAutostart] = useState(false);
  const [err, setErr] = useState("");
  const [loadErr, setLoadErr] = useState("");
  const [saved, setSaved] = useState(false);

  const load = async () => {
    setLoadErr("");
    try {
      const [s, a] = await Promise.all([api().GetSettings(), api().LaunchAtStartupEnabled()]);
      setSettings(s);
      setAutostart(a);
    } catch (e) {
      setLoadErr(String(e));
    }
  };

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const patch = async (p: Partial<Settings>) => {
    if (!settings) return;
    setErr("");
    try {
      const next = { ...settings, ...p };
      await api().SetSettings(next);
      setSettings(next);
      setSaved(true);
      window.setTimeout(() => setSaved(false), 1500);
      await refresh();
    } catch (e) {
      setErr(String(e));
    }
  };

  const setAutostartEnabled = async (on: boolean) => {
    setErr("");
    try {
      if (on) {
        await api().EnableLaunchAtStartup();
      } else {
        await api().DisableLaunchAtStartup();
      }
      setAutostart(on);
    } catch (e) {
      setErr(String(e));
    }
  };

  // Load failure is a real state, not "still loading": show it with a
  // retry. (A bare early return here used to wedge the page on
  // "Loading…" forever when the bridge call raced app startup.)
  if (loadErr) {
    return (
      <div className="mx-auto w-full max-w-2xl px-8 py-8">
        <h1 className="text-lg font-semibold tracking-tight">Settings</h1>
        <p className="mt-4 text-sm text-destructive">Could not load settings: {loadErr}</p>
        <Button className="mt-3" variant="outline" onClick={() => void load()}>
          Retry
        </Button>
      </div>
    );
  }

  if (!settings) {
    return <PageSkeleton rows={2} />;
  }

  return (
    <div className="mx-auto w-full max-w-2xl px-8 py-8">
      <h1 className="text-lg font-semibold tracking-tight">Settings</h1>
      <p className="mt-1 text-sm text-muted-foreground">
        Machine-wide behavior. Role-specific options live on the Server and Client pages.
      </p>

      <Section title="Startup" className="mt-8">
        <Toggle
          checked={autostart}
          onChange={(v) => void setAutostartEnabled(v)}
          title="Launch at startup"
          description="Start kvmshare when you log in (Windows: per-user Run key, manageable from Task Manager; Linux: XDG autostart, honored by every mainstream desktop)."
        />
        <div className="border-t border-border/50 py-3">
          <div className="text-sm">When it launches, start as</div>
          <p className="text-xs text-muted-foreground">
            The role this machine takes automatically at launch. Pick the role each machine plays
            once and never think about it again.
          </p>
          <div className="mt-3 grid gap-2 sm:grid-cols-3">
            {START_ROLES.map((r) => {
              const active = (settings.startRole ?? "") === r.value;
              return (
                <button
                  key={r.value || "none"}
                  onClick={() => void patch({ startRole: r.value })}
                  className={cn(
                    "rounded-lg border px-3 py-2.5 text-left transition-colors",
                    active
                      ? "border-primary bg-primary/5"
                      : "border-border/70 hover:border-border hover:bg-muted/30",
                  )}
                >
                  <div className={cn("text-sm font-medium", active && "text-primary")}>{r.label}</div>
                  <div className="mt-0.5 text-[11px] leading-snug text-muted-foreground">{r.hint}</div>
                </button>
              );
            })}
          </div>
        </div>
        <div className="border-t border-border/50">
          <Toggle
            checked={settings.startHidden ?? false}
            onChange={(v) => void patch({ startHidden: v })}
            title="Start minimized to tray"
            description="When launching at startup, open quietly in the tray instead of showing the window. The tray icon always stays reachable; opening it is one click."
          />
        </div>
      </Section>

      <Section title="Connections" className="mt-6">
        <Toggle
          checked={settings.autoConnect}
          onChange={(v) => void patch({ autoConnect: v })}
          title="Auto-connect"
          description="As a client, connect automatically when a known server appears on the network. Never overrides a stop you made or a server you blocked."
        />
        <div className="border-t border-border/50">
          <Toggle
            checked={settings.acceptPairing}
            onChange={(v) => void patch({ acceptPairing: v })}
            title="Accept connection requests"
            description="Let a discovered server ask this machine to connect. The request is accepted once and remembered; turning this off means every connection must be made from this machine."
          />
        </div>
      </Section>

      <Section title="Diagnostics" className="mt-6">
        <Toggle
          checked={settings.logEnabled}
          onChange={(v) => void patch({ logEnabled: v })}
          title="Logging"
          description="Write role logs for troubleshooting. Disabling silences them entirely."
        />
        {settings.logEnabled && (
          <div className="border-t border-border/50 py-3">
            <div className="text-sm">Log level</div>
            <div className="mt-2 flex flex-wrap gap-1.5">
              {(["error", "warn", "info", "debug", "trace"] as const).map((level) => (
                <Button
                  key={level}
                  size="sm"
                  variant={settings.logLevel === level ? "default" : "outline"}
                  onClick={() => void patch({ logLevel: level })}
                >
                  {level}
                </Button>
              ))}
            </div>
          </div>
        )}
      </Section>

      {(err || saved) && (
        <p className={cn("mt-4 text-xs", err ? "text-destructive" : "text-emerald-600")}>
          {err || "Saved"}
        </p>
      )}
    </div>
  );
}
