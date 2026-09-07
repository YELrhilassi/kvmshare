import { useEffect, useState } from "react";
import { useApp } from "@/app/AppProvider";
import { api, type Paths } from "@/lib/bridge";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import LogViewer from "@/features/logs/LogViewer";
import { cn } from "@/lib/utils";

// The levels the Rust logger accepts, quietest first. "Trace" is the
// very-verbose, per-event level.
const LEVELS = ["error", "warn", "info", "debug", "trace"] as const;

export default function LogsPage() {
  const { mode, running } = useApp();
  const [paths, setPaths] = useState<Paths | null>(null);
  const [level, setLevel] = useState("info");
  const [enabled, setEnabled] = useState(true);
  const [error, setError] = useState("");
  const [clearing, setClearing] = useState(false);

  // The log of *this machine's* instance — server or client, never both.
  const logPath = paths ? (mode === "server" ? paths.serverLog : paths.clientLog) : undefined;

  useEffect(() => {
    void api()
      .GetPaths()
      .then(setPaths)
      .catch(() => {});
    void api()
      .GetLogSettings()
      .then((s) => {
        setLevel(s.level);
        setEnabled(s.enabled);
      })
      .catch(() => {});
  }, []);

  const apply = async (nextLevel: string, nextEnabled: boolean) => {
    setError("");
    try {
      await api().SetLogSettings({ role: mode, level: nextLevel, enabled: nextEnabled });
      setLevel(nextLevel);
      setEnabled(nextEnabled);
    } catch (e) {
      setError(String(e));
    }
  };

  const clear = async () => {
    setClearing(true);
    setError("");
    try {
      await api().ClearLog(mode);
    } catch (e) {
      setError(String(e));
    } finally {
      setClearing(false);
    }
  };

  const active = running[mode];

  return (
    <div className="flex h-full flex-col">
      <div className="mx-auto w-full max-w-5xl px-10 pt-10">
        <header className="mb-6 flex items-end justify-between gap-6">
          <div className="space-y-1">
            <h1 className="text-2xl font-semibold tracking-tight">Logs</h1>
            <p className="text-sm text-muted-foreground">
              <span className="font-medium text-foreground/80">
                {mode === "server" ? "Server" : "Client"} instance
              </span>{" "}
              · this machine&apos;s own log
              <span className="ml-2 inline-flex items-center gap-1.5">
                <span
                  className={cn("h-2 w-2 rounded-full", active ? "bg-emerald-500" : "bg-muted-foreground/40")}
                />
                {active ? "running" : "not running"}
              </span>
            </p>
          </div>

          <div className="flex items-center gap-2">
            <select
              value={level}
              onChange={(e) => void apply(e.target.value, enabled)}
              className="h-8 rounded-md border border-border/70 bg-muted/40 px-2 text-xs font-medium text-foreground outline-none transition-colors focus:border-primary"
            >
              {LEVELS.map((l) => (
                <option key={l} value={l}>
                  {l === "trace" ? "trace · very verbose" : l}
                </option>
              ))}
            </select>
            <label className="flex h-8 cursor-pointer items-center gap-2 rounded-md px-2 text-xs text-muted-foreground transition-colors hover:text-foreground">
              <Switch checked={enabled} onCheckedChange={(on) => void apply(level, on)} />
              logging
            </label>
            <Button variant="outline" size="sm" onClick={clear} disabled={clearing}>
              {clearing ? "Clearing…" : "Clear"}
            </Button>
          </div>
        </header>
        {error && <p className="mb-4 text-xs text-destructive">{error}</p>}
        {!enabled && (
          <p className="mb-4 text-xs text-muted-foreground/70">
            Logging is disabled — no lines are written until it is turned back on (applies live, no restart).
          </p>
        )}
      </div>

      <div className="mx-auto flex min-h-0 w-full max-w-5xl flex-1 flex-col px-10 pb-10">
        <LogViewer path={logPath} />
      </div>
    </div>
  );
}