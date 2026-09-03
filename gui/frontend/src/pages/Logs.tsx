import { useEffect, useMemo, useState } from "react";
import { api, type Mode, type Paths } from "@/lib/bridge";
import { useLogTail, useRunning } from "@/lib/hooks";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";

// The levels the Rust logger accepts, quietest first. "Trace" is the
// very-verbose, per-event level.
const LEVELS = ["error", "warn", "info", "debug", "trace"] as const;

function lineClass(level: string): string {
  switch (level) {
    case "ERROR":
      return "text-red-400";
    case "WARN":
      return "text-amber-400";
    case "DEBUG":
      return "text-muted-foreground/75";
    case "TRACE":
      return "text-muted-foreground/50";
    default:
      return "";
  }
}

interface Props {
  mode: Mode;
}

export default function LogsPage({ mode }: Props) {
  const running = useRunning();
  const [paths, setPaths] = useState<Paths | null>(null);
  const [level, setLevel] = useState("info");
  const [enabled, setEnabled] = useState(true);
  const [error, setError] = useState("");
  const [clearing, setClearing] = useState(false);

  // The log of *this machine's* instance — server or client, never both.
  const logPath = paths ? (mode === "server" ? paths.serverLog : paths.clientLog) : undefined;
  const { log, viewportRef, onScroll, stick, setStick } = useLogTail(logPath, 500);

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

  // One colored line per log line; a plain line when there is nothing yet.
  const lines = useMemo(() => {
    if (!log) return null;
    return log.split("\n").map((line, i) => {
      // Lines are `HH:MM:SS LEVEL component: message` — pull the level.
      const level = line.split(" ")[1];
      return (
        <div key={i} className={cn("min-w-max", lineClass(level))}>
          {line}
        </div>
      );
    });
  }, [log]);

  const active = mode === "server" ? running.server : running.client;

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

      <div className="mx-auto min-h-0 w-full max-w-5xl flex-1 px-10 pb-10">
        <div className="flex h-full flex-col overflow-hidden rounded-md border border-border/70 bg-muted/20">
          <div className="flex shrink-0 items-center justify-between border-b border-border/60 px-4 py-2 text-[11px] text-muted-foreground/70">
            <span className="font-mono">{logPath ?? "…"}</span>
            <button
              onClick={() => setStick(!stick)}
              className={cn(
                "transition-colors hover:text-foreground",
                stick && "text-foreground",
              )}
            >
              {stick ? "following" : "follow"}
            </button>
          </div>
          <div
            ref={viewportRef}
            onScroll={onScroll}
            className="min-h-0 flex-1 overflow-y-auto p-4 font-mono text-xs leading-relaxed whitespace-pre-wrap"
          >
            {lines ?? <span className="text-muted-foreground/50">— no log output yet —</span>}
          </div>
        </div>
      </div>
    </div>
  );
}