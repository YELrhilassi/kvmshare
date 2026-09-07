import { useMemo } from "react";
import { useLogTail } from "@/lib/useLogTail";
import { cn } from "@/lib/utils";

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

// Live tail of one log file. Lines are `HH:MM:SS LEVEL component: msg`;
// the level is pulled out for coloring.
export default function LogViewer({ path, maxLines = 500 }: { path: string | undefined; maxLines?: number }) {
  const { log, viewportRef, onScroll, stick, setStick } = useLogTail(path, maxLines);

  const lines = useMemo(() => {
    if (!log) return null;
    return log.split("\n").map((line, i) => {
      const level = line.split(" ")[1];
      return (
        <div key={i} className={cn("min-w-max", lineClass(level))}>
          {line}
        </div>
      );
    });
  }, [log]);

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden border border-border/60 bg-muted/20">
      <div className="flex shrink-0 items-center justify-between border-b border-border/60 px-4 py-2 text-[11px] text-muted-foreground/70">
        <span className="truncate font-mono">{path ?? "…"}</span>
        <button
          onClick={() => setStick(!stick)}
          className={cn("shrink-0 transition-colors hover:text-foreground", stick && "text-foreground")}
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
  );
}